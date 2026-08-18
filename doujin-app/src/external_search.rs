use doujin_files::RecycleBin;
use doujin_parser::domain::Identifier;
use doujin_storage::StorageError;
use doujin_storage::collections::CollectionSnapshot;
use doujin_storage::external_search_batches::{
    ExternalSearchBatchItemOutcome, ExternalSearchBatchSnapshot, ExternalSearchBatchStrategy,
    NewExternalSearchBatchItem,
};
use doujin_storage::jobs::{
    ExternalSearchActivityItem, ExternalSearchActivitySnapshot, ExternalSearchCompletionStatus,
    ExternalSearchEnqueueOutcome, ExternalSearchErrorKind, ExternalSearchJobIssue,
    ExternalSearchJobSnapshot, ExternalSearchJobStatus, ExternalSearchJobSummary,
};
use doujin_storage::metadata::{
    ConfidenceEvidence, ExternalCandidate, ExternalCandidateOutcome, ExternalTag,
    ExternalTagOutcome, MetadataField, MetadataValue,
};

use crate::{ApplicationResult, ApplicationService};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchRequest {
    pub job_id: i64,
    pub collection: CollectionSnapshot,
    pub identifiers: Vec<Identifier>,
    pub fields: Vec<MetadataField>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExternalSearchStartOutcome {
    Ready(Box<ExternalSearchRequest>),
    Finished(ExternalSearchJobSnapshot),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalMetadataCandidate {
    pub field: MetadataField,
    pub value: MetadataValue,
    pub source_reference: String,
    pub confidence: ConfidenceEvidence,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalTagCandidate {
    pub name: String,
    pub source_reference: String,
    pub confidence: ConfidenceEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchProviderIssue {
    pub field: Option<MetadataField>,
    pub kind: ExternalSearchErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSearchProviderResponse {
    pub candidates: Vec<ExternalMetadataCandidate>,
    pub tags: Vec<ExternalTagCandidate>,
    pub issues: Vec<ExternalSearchProviderIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchProviderError {
    pub kind: ExternalSearchErrorKind,
    pub message: String,
}

pub trait ExternalMetadataProvider {
    fn search(
        &self,
        request: &ExternalSearchRequest,
    ) -> Result<ExternalSearchProviderResponse, ExternalSearchProviderError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchWorkerIssue {
    pub job_id: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchWorkerReport {
    pub due: usize,
    pub processed: usize,
    pub succeeded: usize,
    pub partial: usize,
    pub retry_scheduled: usize,
    pub failed: usize,
    pub issues: Vec<ExternalSearchWorkerIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchBatchFieldNeed {
    pub field: MetadataField,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchBatchPreflightItem {
    pub collection_id: i64,
    pub fields: Vec<MetadataField>,
    pub outcome: ExternalSearchBatchItemOutcome,
    pub job_id: Option<i64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchBatchPreflight {
    pub strategy: ExternalSearchBatchStrategy,
    pub fields: Vec<MetadataField>,
    pub total: usize,
    pub will_enqueue: usize,
    pub reused: usize,
    pub skipped: usize,
    pub unchanged: usize,
    pub insufficient_identifiers: usize,
    pub field_needs: Vec<ExternalSearchBatchFieldNeed>,
    pub items: Vec<ExternalSearchBatchPreflightItem>,
}

impl<R: RecycleBin> ApplicationService<R> {
    pub fn preflight_external_search_batch(
        &self,
        collection_ids: &[i64],
        fields: &[MetadataField],
        strategy: ExternalSearchBatchStrategy,
    ) -> ApplicationResult<ExternalSearchBatchPreflight> {
        let collection_ids = normalized_collection_ids(collection_ids)?;
        let fields = normalized_batch_fields(fields)?;
        let mut field_needs = MetadataField::ALL
            .into_iter()
            .filter(|field| fields.contains(field))
            .map(|field| ExternalSearchBatchFieldNeed { field, count: 0 })
            .collect::<Vec<_>>();
        let mut items = Vec::with_capacity(collection_ids.len());
        let mut will_enqueue = 0;
        let mut reused = 0;
        let mut skipped = 0;
        let mut unchanged = 0;
        let mut insufficient_identifiers = 0;
        for collection_id in collection_ids {
            let collection = match self.repository.collection(collection_id) {
                Ok(collection) => collection,
                Err(StorageError::CollectionNotFound(_)) => {
                    skipped += 1;
                    items.push(ExternalSearchBatchPreflightItem {
                        collection_id,
                        fields: Vec::new(),
                        outcome: ExternalSearchBatchItemOutcome::Skipped,
                        job_id: None,
                        reason: Some("收藏不存在或已不在 active library".to_owned()),
                    });
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let requested = fields
                .iter()
                .copied()
                .filter(|field| {
                    strategy == ExternalSearchBatchStrategy::Specified
                        || metadata_field_missing(&collection, *field)
                })
                .collect::<Vec<_>>();
            if requested.is_empty() {
                unchanged += 1;
                items.push(ExternalSearchBatchPreflightItem {
                    collection_id,
                    fields: requested,
                    outcome: ExternalSearchBatchItemOutcome::Unchanged,
                    job_id: None,
                    reason: Some("指定欄位已有 metadata".to_owned()),
                });
                continue;
            }
            for need in &mut field_needs {
                if requested.contains(&need.field) {
                    need.count += 1;
                }
            }
            if let Some(job) = self.repository.active_external_search_job(collection_id)? {
                reused += 1;
                items.push(ExternalSearchBatchPreflightItem {
                    collection_id,
                    fields: requested,
                    outcome: ExternalSearchBatchItemOutcome::Reused,
                    job_id: Some(job.id),
                    reason: Some("已有 pending/running external search job".to_owned()),
                });
                continue;
            }
            let identifiers = self.repository.latest_parser_identifiers(collection_id)?;
            let has_title = collection
                .title
                .as_deref()
                .is_some_and(|title| !title.trim().is_empty());
            if identifiers.is_empty() && !has_title {
                skipped += 1;
                insufficient_identifiers += 1;
                items.push(ExternalSearchBatchPreflightItem {
                    collection_id,
                    fields: requested,
                    outcome: ExternalSearchBatchItemOutcome::Skipped,
                    job_id: None,
                    reason: Some("缺少 provider 可用的識別碼或辨識書名".to_owned()),
                });
                continue;
            }
            will_enqueue += 1;
            items.push(ExternalSearchBatchPreflightItem {
                collection_id,
                fields: requested,
                outcome: ExternalSearchBatchItemOutcome::Enqueued,
                job_id: None,
                reason: None,
            });
        }
        Ok(ExternalSearchBatchPreflight {
            strategy,
            fields,
            total: items.len(),
            will_enqueue,
            reused,
            skipped,
            unchanged,
            insufficient_identifiers,
            field_needs,
            items,
        })
    }

    pub fn create_external_search_batch(
        &mut self,
        collection_ids: &[i64],
        fields: &[MetadataField],
        strategy: ExternalSearchBatchStrategy,
    ) -> ApplicationResult<ExternalSearchBatchSnapshot> {
        let preflight = self.preflight_external_search_batch(collection_ids, fields, strategy)?;
        let mut stored_items = Vec::with_capacity(preflight.items.len());
        for item in preflight.items {
            let (job_id, outcome, reason) = match item.outcome {
                ExternalSearchBatchItemOutcome::Enqueued => {
                    let result = self.enqueue_external_search(item.collection_id, &item.fields)?;
                    if result.created {
                        (
                            Some(result.job.id),
                            ExternalSearchBatchItemOutcome::Enqueued,
                            None,
                        )
                    } else {
                        (
                            Some(result.job.id),
                            ExternalSearchBatchItemOutcome::Reused,
                            Some(
                                "建立 batch 時已有 pending/running external search job".to_owned(),
                            ),
                        )
                    }
                }
                _ => (item.job_id, item.outcome, item.reason),
            };
            stored_items.push(NewExternalSearchBatchItem {
                collection_id: item.collection_id,
                job_id,
                outcome,
                fields: item.fields,
                reason,
            });
        }
        Ok(self.repository.create_external_search_batch(
            preflight.strategy,
            &preflight.fields,
            &stored_items,
        )?)
    }

    pub fn external_search_batch(
        &self,
        batch_id: i64,
    ) -> ApplicationResult<ExternalSearchBatchSnapshot> {
        Ok(self.repository.external_search_batch(batch_id)?)
    }

    pub fn retry_external_search_batch(
        &mut self,
        batch_id: i64,
    ) -> ApplicationResult<ExternalSearchBatchSnapshot> {
        let batch = self.repository.external_search_batch(batch_id)?;
        let retryable = batch
            .items
            .iter()
            .filter(|item| item.job_status == Some(ExternalSearchJobStatus::Partial))
            .map(|item| item.collection_id)
            .collect::<Vec<_>>();
        if retryable.is_empty() {
            return Err(StorageError::InvalidExternalSearchBatch(
                "沒有可重試項目；typed permanent failure 不可由 batch 繞過".to_owned(),
            )
            .into());
        }
        self.create_external_search_batch(
            &retryable,
            &batch.fields,
            ExternalSearchBatchStrategy::Specified,
        )
    }

    pub fn recover_interrupted_external_search_jobs(&mut self) -> ApplicationResult<usize> {
        Ok(self.repository.recover_interrupted_external_search_jobs()?)
    }

    pub fn run_due_external_search_jobs<P: ExternalMetadataProvider>(
        &mut self,
        provider: &P,
        limit: u32,
    ) -> ApplicationResult<ExternalSearchWorkerReport> {
        let due_jobs = self.repository.due_external_search_jobs(limit)?;
        let mut report = ExternalSearchWorkerReport {
            due: due_jobs.len(),
            processed: 0,
            succeeded: 0,
            partial: 0,
            retry_scheduled: 0,
            failed: 0,
            issues: Vec::new(),
        };
        for due_job in due_jobs {
            match self.run_external_search_job(due_job.id, provider) {
                Ok(job) => {
                    report.processed += 1;
                    match job.status {
                        ExternalSearchJobStatus::Succeeded => report.succeeded += 1,
                        ExternalSearchJobStatus::Partial => report.partial += 1,
                        ExternalSearchJobStatus::Pending => report.retry_scheduled += 1,
                        ExternalSearchJobStatus::Failed => report.failed += 1,
                        ExternalSearchJobStatus::Running => {
                            report.issues.push(ExternalSearchWorkerIssue {
                                job_id: job.id,
                                message: "工作執行後仍處於 running".to_owned(),
                            });
                        }
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    let requeued = self
                        .repository
                        .requeue_interrupted_external_search_job(due_job.id, &message);
                    report.issues.push(ExternalSearchWorkerIssue {
                        job_id: due_job.id,
                        message: match requeued {
                            Ok(_) => message,
                            Err(requeue_error) => {
                                format!("{message}；工作回復失敗：{requeue_error}")
                            }
                        },
                    });
                }
            }
        }
        Ok(report)
    }

    pub fn due_external_search_jobs(
        &self,
        limit: u32,
    ) -> ApplicationResult<Vec<ExternalSearchJobSnapshot>> {
        Ok(self.repository.due_external_search_jobs(limit)?)
    }

    pub fn enqueue_external_search(
        &mut self,
        collection_id: i64,
        fields: &[MetadataField],
    ) -> ApplicationResult<ExternalSearchEnqueueOutcome> {
        self.repository.collection(collection_id)?;
        Ok(self
            .repository
            .enqueue_external_search(collection_id, fields)?)
    }

    pub fn external_search_job(&self, job_id: i64) -> ApplicationResult<ExternalSearchJobSnapshot> {
        let job = self.repository.external_search_job(job_id)?;
        self.repository.collection(job.collection_id)?;
        Ok(job)
    }

    pub fn external_search_activity(&self) -> ApplicationResult<ExternalSearchActivitySnapshot> {
        Ok(self.repository.external_search_activity()?)
    }

    pub fn acknowledge_external_search_job(
        &mut self,
        job_id: i64,
    ) -> ApplicationResult<ExternalSearchActivityItem> {
        let job = self.repository.external_search_job(job_id)?;
        self.repository.collection(job.collection_id)?;
        Ok(self.repository.acknowledge_external_search_job(job_id)?)
    }

    pub fn run_external_search_job<P: ExternalMetadataProvider>(
        &mut self,
        job_id: i64,
        provider: &P,
    ) -> ApplicationResult<ExternalSearchJobSnapshot> {
        let request = match self.start_external_search_job(job_id)? {
            ExternalSearchStartOutcome::Ready(request) => request,
            ExternalSearchStartOutcome::Finished(job) => return Ok(job),
        };
        let response = provider.search(&request);
        self.finish_external_search_job(job_id, response)
    }

    pub fn start_external_search_job(
        &mut self,
        job_id: i64,
    ) -> ApplicationResult<ExternalSearchStartOutcome> {
        let running = self.repository.start_external_search_job(job_id)?;
        let collection = match self.repository.collection(running.collection_id) {
            Ok(collection) => collection,
            Err(StorageError::CollectionNotFound(_)) => {
                return Ok(ExternalSearchStartOutcome::Finished(
                    self.repository.fail_external_search_job(
                        job_id,
                        ExternalSearchErrorKind::Unsupported,
                        "收藏已不存在或不是 active，取消外部搜尋",
                        None,
                    )?,
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let identifiers = self
            .repository
            .latest_parser_identifiers(running.collection_id)?;
        Ok(ExternalSearchStartOutcome::Ready(Box::new(
            ExternalSearchRequest {
                job_id,
                collection,
                identifiers,
                fields: running.fields.clone(),
            },
        )))
    }

    pub fn finish_external_search_job(
        &mut self,
        job_id: i64,
        response: Result<ExternalSearchProviderResponse, ExternalSearchProviderError>,
    ) -> ApplicationResult<ExternalSearchJobSnapshot> {
        let running = self.repository.external_search_job(job_id)?;
        if running.status != ExternalSearchJobStatus::Running {
            return Err(StorageError::ExternalSearchJobUnavailable(job_id).into());
        }
        if matches!(
            self.repository.collection(running.collection_id),
            Err(StorageError::CollectionNotFound(_))
        ) {
            return Ok(self.repository.fail_external_search_job(
                job_id,
                ExternalSearchErrorKind::Unsupported,
                "收藏已不存在或不是 active，取消外部搜尋",
                None,
            )?);
        }
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let message = nonempty_message(error.message, "external metadata provider 失敗");
                return Ok(self
                    .repository
                    .fail_external_search_job(job_id, error.kind, &message, None)?);
            }
        };
        self.persist_external_search_response(&running, response)
    }

    pub fn requeue_interrupted_external_search_job(
        &mut self,
        job_id: i64,
        message: &str,
    ) -> ApplicationResult<ExternalSearchJobSnapshot> {
        Ok(self
            .repository
            .requeue_interrupted_external_search_job(job_id, message)?)
    }

    fn persist_external_search_response(
        &mut self,
        job: &ExternalSearchJobSnapshot,
        response: ExternalSearchProviderResponse,
    ) -> ApplicationResult<ExternalSearchJobSnapshot> {
        let candidates_received = response.candidates.len();
        let tags_received = response.tags.len();
        let mut auto_applied = 0;
        let mut suggestions = 0;
        let mut suggestion_assertion_ids = Vec::new();
        let mut search_only = 0;
        let mut tags_applied = 0;
        let mut issues = response
            .issues
            .into_iter()
            .map(|issue| ExternalSearchJobIssue {
                field: issue.field,
                kind: issue.kind.as_str().to_owned(),
                message: nonempty_message(issue.message, "provider 未提供錯誤訊息"),
            })
            .collect::<Vec<_>>();

        for candidate in response.candidates {
            if !job.fields.contains(&candidate.field) {
                issues.push(ExternalSearchJobIssue {
                    field: Some(candidate.field),
                    kind: ExternalSearchErrorKind::InvalidResponse.as_str().to_owned(),
                    message: "provider 回傳未要求的 metadata field".to_owned(),
                });
                continue;
            }
            let outcome = self.repository.save_external_candidate(ExternalCandidate {
                collection_id: job.collection_id,
                field: candidate.field,
                value: candidate.value,
                source_reference: candidate.source_reference,
                confidence: candidate.confidence,
            });
            match outcome {
                Ok(ExternalCandidateOutcome::AutoApplied { .. }) => auto_applied += 1,
                Ok(ExternalCandidateOutcome::Suggestion { assertion_id, .. }) => {
                    suggestions += 1;
                    suggestion_assertion_ids.push(assertion_id);
                }
                Ok(ExternalCandidateOutcome::SearchOnly { .. }) => search_only += 1,
                Err(StorageError::InvalidMetadata(reason)) => {
                    issues.push(ExternalSearchJobIssue {
                        field: Some(candidate.field),
                        kind: ExternalSearchErrorKind::InvalidResponse.as_str().to_owned(),
                        message: reason,
                    });
                }
                Err(error) => return Err(error.into()),
            }
        }

        for tag in response.tags {
            let outcome = self.repository.save_external_tag(ExternalTag {
                collection_id: job.collection_id,
                name: tag.name,
                source_reference: tag.source_reference,
                confidence: tag.confidence,
            });
            match outcome {
                Ok(ExternalTagOutcome::Applied { .. }) => tags_applied += 1,
                Ok(ExternalTagOutcome::Existing { .. }) => {}
                Err(StorageError::InvalidMetadata(reason)) => {
                    issues.push(ExternalSearchJobIssue {
                        field: None,
                        kind: ExternalSearchErrorKind::InvalidResponse.as_str().to_owned(),
                        message: reason,
                    });
                }
                Err(error) => return Err(error.into()),
            }
        }

        let summary = ExternalSearchJobSummary {
            candidates_received,
            tags_received,
            tags_applied,
            auto_applied,
            suggestions,
            suggestion_assertion_ids,
            search_only,
            issues,
        };
        let stored = auto_applied + suggestions + search_only + tags_received;
        if stored == 0 && !summary.issues.is_empty() {
            let error_kind = summary
                .issues
                .iter()
                .filter_map(|issue| parse_error_kind(&issue.kind))
                .find(|kind| kind.retry_delay_seconds(job.attempts).is_some())
                .or_else(|| {
                    summary
                        .issues
                        .iter()
                        .find_map(|issue| parse_error_kind(&issue.kind))
                })
                .unwrap_or(ExternalSearchErrorKind::InvalidResponse);
            let message = summary
                .issues
                .iter()
                .map(|issue| issue.message.as_str())
                .collect::<Vec<_>>()
                .join("；");
            return Ok(self.repository.fail_external_search_job(
                job.id,
                error_kind,
                &message,
                Some(&summary),
            )?);
        }
        if stored == 0 {
            return Ok(self.repository.fail_external_search_job(
                job.id,
                ExternalSearchErrorKind::NoMatch,
                "provider 沒有回傳任何候選",
                Some(&summary),
            )?);
        }
        let status = if summary.issues.is_empty() {
            ExternalSearchCompletionStatus::Succeeded
        } else {
            ExternalSearchCompletionStatus::Partial
        };
        Ok(self
            .repository
            .complete_external_search_job(job.id, status, &summary)?)
    }
}

fn normalized_collection_ids(collection_ids: &[i64]) -> Result<Vec<i64>, StorageError> {
    let mut normalized = Vec::with_capacity(collection_ids.len());
    for collection_id in collection_ids {
        if *collection_id <= 0 {
            return Err(StorageError::InvalidExternalSearchBatch(
                "collection IDs 必須是正整數".to_owned(),
            ));
        }
        if !normalized.contains(collection_id) {
            normalized.push(*collection_id);
        }
    }
    if normalized.is_empty() {
        return Err(StorageError::InvalidExternalSearchBatch(
            "batch 至少必須包含一筆 collection".to_owned(),
        ));
    }
    Ok(normalized)
}

fn normalized_batch_fields(fields: &[MetadataField]) -> Result<Vec<MetadataField>, StorageError> {
    let fields = MetadataField::ALL
        .into_iter()
        .filter(|field| fields.contains(field))
        .collect::<Vec<_>>();
    if fields.is_empty() {
        return Err(StorageError::InvalidExternalSearchBatch(
            "batch 至少必須指定一個 metadata field".to_owned(),
        ));
    }
    Ok(fields)
}

fn metadata_field_missing(collection: &CollectionSnapshot, field: MetadataField) -> bool {
    match field {
        MetadataField::Title => collection.title.as_deref().is_none_or(str::is_empty),
        MetadataField::Event => collection.event.as_deref().is_none_or(str::is_empty),
        MetadataField::Circle => collection.circle.as_deref().is_none_or(str::is_empty),
        MetadataField::Authors => collection.authors.is_empty(),
        MetadataField::Parody => collection.parody.as_deref().is_none_or(str::is_empty),
        MetadataField::Classification => collection
            .classification_top
            .as_deref()
            .is_none_or(str::is_empty),
        MetadataField::IsDl => collection.is_dl.is_none(),
    }
}

fn parse_error_kind(value: &str) -> Option<ExternalSearchErrorKind> {
    match value {
        "network" => Some(ExternalSearchErrorKind::Network),
        "rate_limited" => Some(ExternalSearchErrorKind::RateLimited),
        "provider_unavailable" => Some(ExternalSearchErrorKind::ProviderUnavailable),
        "worker_interrupted" => Some(ExternalSearchErrorKind::WorkerInterrupted),
        "invalid_response" => Some(ExternalSearchErrorKind::InvalidResponse),
        "no_match" => Some(ExternalSearchErrorKind::NoMatch),
        "unsupported" => Some(ExternalSearchErrorKind::Unsupported),
        _ => None,
    }
}

fn nonempty_message(message: String, fallback: &str) -> String {
    let message = message.trim();
    if message.is_empty() {
        fallback.to_owned()
    } else {
        message.to_owned()
    }
}
