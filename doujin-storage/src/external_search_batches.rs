use rusqlite::{Row, params};

use crate::jobs::ExternalSearchJobStatus;
use crate::metadata::MetadataField;
use crate::{CatalogRepository, StorageError, StorageResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSearchBatchStrategy {
    OnlyMissing,
    Specified,
}

impl ExternalSearchBatchStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OnlyMissing => "only_missing",
            Self::Specified => "specified",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "only_missing" => Ok(Self::OnlyMissing),
            "specified" => Ok(Self::Specified),
            _ => Err(StorageError::InvalidSchema(format!(
                "未知 external search batch strategy：{value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSearchBatchItemOutcome {
    Enqueued,
    Reused,
    Skipped,
    Unchanged,
}

impl ExternalSearchBatchItemOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enqueued => "enqueued",
            Self::Reused => "reused",
            Self::Skipped => "skipped",
            Self::Unchanged => "unchanged",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "enqueued" => Ok(Self::Enqueued),
            "reused" => Ok(Self::Reused),
            "skipped" => Ok(Self::Skipped),
            "unchanged" => Ok(Self::Unchanged),
            _ => Err(StorageError::InvalidSchema(format!(
                "未知 external search batch item outcome：{value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExternalSearchBatchItem {
    pub collection_id: i64,
    pub job_id: Option<i64>,
    pub outcome: ExternalSearchBatchItemOutcome,
    pub fields: Vec<MetadataField>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchBatchItemSnapshot {
    pub collection_id: i64,
    pub job_id: Option<i64>,
    pub outcome: ExternalSearchBatchItemOutcome,
    pub fields: Vec<MetadataField>,
    pub reason: Option<String>,
    pub job_status: Option<ExternalSearchJobStatus>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub next_retry_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExternalSearchBatchSummary {
    pub total: usize,
    pub pending: usize,
    pub running: usize,
    pub succeeded: usize,
    pub partial: usize,
    pub failed: usize,
    pub skipped: usize,
    pub unchanged: usize,
    pub reused: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchBatchSnapshot {
    pub id: i64,
    pub strategy: ExternalSearchBatchStrategy,
    pub fields: Vec<MetadataField>,
    pub created_at: String,
    pub summary: ExternalSearchBatchSummary,
    pub items: Vec<ExternalSearchBatchItemSnapshot>,
}

impl CatalogRepository {
    pub fn create_external_search_batch(
        &mut self,
        strategy: ExternalSearchBatchStrategy,
        fields: &[MetadataField],
        items: &[NewExternalSearchBatchItem],
    ) -> StorageResult<ExternalSearchBatchSnapshot> {
        let fields_json = encode_fields(fields, false)?;
        if items.is_empty() {
            return Err(StorageError::InvalidExternalSearchBatch(
                "batch 至少必須包含一筆 collection".to_owned(),
            ));
        }
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO external_search_batches(strategy, fields_json) VALUES (?1, ?2)",
            params![strategy.as_str(), fields_json],
        )?;
        let batch_id = transaction.last_insert_rowid();
        for item in items {
            let item_fields = encode_fields(&item.fields, true)?;
            let reason = item
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|reason| !reason.is_empty());
            let valid_link = matches!(
                (item.outcome, item.job_id),
                (
                    ExternalSearchBatchItemOutcome::Enqueued
                        | ExternalSearchBatchItemOutcome::Reused,
                    Some(_)
                ) | (
                    ExternalSearchBatchItemOutcome::Skipped
                        | ExternalSearchBatchItemOutcome::Unchanged,
                    None
                )
            );
            if !valid_link {
                return Err(StorageError::InvalidExternalSearchBatch(
                    "batch item outcome 與 job reference 不一致".to_owned(),
                ));
            }
            transaction.execute(
                "INSERT INTO external_search_batch_items(
                    batch_id, collection_id, job_id, outcome, fields_json, reason
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    batch_id,
                    item.collection_id,
                    item.job_id,
                    item.outcome.as_str(),
                    item_fields,
                    reason
                ],
            )?;
        }
        transaction.commit()?;
        self.external_search_batch(batch_id)
    }

    pub fn external_search_batch(
        &self,
        batch_id: i64,
    ) -> StorageResult<ExternalSearchBatchSnapshot> {
        let (strategy, fields_json, created_at) = self
            .connection
            .query_row(
                "SELECT strategy, fields_json, created_at
                 FROM external_search_batches WHERE id = ?1",
                [batch_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => {
                    StorageError::ExternalSearchBatchNotFound(batch_id)
                }
                error => StorageError::Sqlite(error),
            })?;
        let mut statement = self.connection.prepare(
            "SELECT item.collection_id, item.job_id, item.outcome, item.fields_json, item.reason,
                    job.status, job.error_kind, job.error_message, job.next_retry_at
             FROM external_search_batch_items AS item
             LEFT JOIN background_jobs AS job ON job.id = item.job_id
             WHERE item.batch_id = ?1 ORDER BY item.id",
        )?;
        let rows = statement
            .query_map([batch_id], raw_batch_item)?
            .collect::<Result<Vec<_>, _>>()?;
        let items = rows
            .into_iter()
            .map(decode_batch_item)
            .collect::<StorageResult<Vec<_>>>()?;
        let mut summary = ExternalSearchBatchSummary {
            total: items.len(),
            ..ExternalSearchBatchSummary::default()
        };
        for item in &items {
            match item.outcome {
                ExternalSearchBatchItemOutcome::Skipped => summary.skipped += 1,
                ExternalSearchBatchItemOutcome::Unchanged => summary.unchanged += 1,
                ExternalSearchBatchItemOutcome::Reused => summary.reused += 1,
                ExternalSearchBatchItemOutcome::Enqueued => {}
            }
            match item.job_status {
                Some(ExternalSearchJobStatus::Pending) => summary.pending += 1,
                Some(ExternalSearchJobStatus::Running) => summary.running += 1,
                Some(ExternalSearchJobStatus::Succeeded) => summary.succeeded += 1,
                Some(ExternalSearchJobStatus::Partial) => summary.partial += 1,
                Some(ExternalSearchJobStatus::Failed) => summary.failed += 1,
                None => {}
            }
        }
        Ok(ExternalSearchBatchSnapshot {
            id: batch_id,
            strategy: ExternalSearchBatchStrategy::parse(&strategy)?,
            fields: decode_fields(&fields_json, false)?,
            created_at,
            summary,
            items,
        })
    }
}

struct RawBatchItem {
    collection_id: i64,
    job_id: Option<i64>,
    outcome: String,
    fields_json: String,
    reason: Option<String>,
    job_status: Option<String>,
    error_kind: Option<String>,
    error_message: Option<String>,
    next_retry_at: Option<String>,
}

fn raw_batch_item(row: &Row<'_>) -> rusqlite::Result<RawBatchItem> {
    Ok(RawBatchItem {
        collection_id: row.get(0)?,
        job_id: row.get(1)?,
        outcome: row.get(2)?,
        fields_json: row.get(3)?,
        reason: row.get(4)?,
        job_status: row.get(5)?,
        error_kind: row.get(6)?,
        error_message: row.get(7)?,
        next_retry_at: row.get(8)?,
    })
}

fn decode_batch_item(raw: RawBatchItem) -> StorageResult<ExternalSearchBatchItemSnapshot> {
    Ok(ExternalSearchBatchItemSnapshot {
        collection_id: raw.collection_id,
        job_id: raw.job_id,
        outcome: ExternalSearchBatchItemOutcome::parse(&raw.outcome)?,
        fields: decode_fields(&raw.fields_json, true)?,
        reason: raw.reason,
        job_status: raw
            .job_status
            .as_deref()
            .map(|status| match status {
                "pending" => Ok(ExternalSearchJobStatus::Pending),
                "running" => Ok(ExternalSearchJobStatus::Running),
                "succeeded" => Ok(ExternalSearchJobStatus::Succeeded),
                "partial" => Ok(ExternalSearchJobStatus::Partial),
                "failed" => Ok(ExternalSearchJobStatus::Failed),
                _ => Err(StorageError::InvalidSchema(format!(
                    "未知 external search job status：{status}"
                ))),
            })
            .transpose()?,
        error_kind: raw.error_kind,
        error_message: raw.error_message,
        next_retry_at: raw.next_retry_at,
    })
}

fn encode_fields(fields: &[MetadataField], allow_empty: bool) -> StorageResult<String> {
    let normalized = MetadataField::ALL
        .into_iter()
        .filter(|field| fields.contains(field))
        .collect::<Vec<_>>();
    if normalized.is_empty() && !allow_empty {
        return Err(StorageError::InvalidExternalSearchBatch(
            "batch 至少必須指定一個 metadata field".to_owned(),
        ));
    }
    Ok(serde_json::to_string(
        &normalized
            .into_iter()
            .map(MetadataField::as_str)
            .collect::<Vec<_>>(),
    )?)
}

fn decode_fields(value: &str, allow_empty: bool) -> StorageResult<Vec<MetadataField>> {
    let values: Vec<String> = serde_json::from_str(value)?;
    let mut fields = Vec::with_capacity(values.len());
    for value in values {
        let field = MetadataField::parse(&value).map_err(StorageError::InvalidSchema)?;
        if !fields.contains(&field) {
            fields.push(field);
        }
    }
    if fields.is_empty() && !allow_empty {
        return Err(StorageError::InvalidSchema(
            "external search batch 至少必須有一個 field".to_owned(),
        ));
    }
    Ok(fields)
}
