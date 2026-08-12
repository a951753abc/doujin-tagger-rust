use rusqlite::{OptionalExtension, Row, params};

use crate::metadata::MetadataField;
use crate::{CatalogRepository, StorageError, StorageResult};

const EXTERNAL_SEARCH_JOB_COLUMNS: &str =
    "id, collection_id, status, payload_json, result_json, error_kind, error_message, attempts,
     next_retry_at, created_at, updated_at";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSearchJobStatus {
    Pending,
    Running,
    Succeeded,
    Partial,
    Failed,
}

impl ExternalSearchJobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "partial" => Ok(Self::Partial),
            "failed" => Ok(Self::Failed),
            _ => Err(format!("未知 external search job status：{value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSearchErrorKind {
    Network,
    RateLimited,
    ProviderUnavailable,
    WorkerInterrupted,
    InvalidResponse,
    NoMatch,
    Unsupported,
}

impl ExternalSearchErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::RateLimited => "rate_limited",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::WorkerInterrupted => "worker_interrupted",
            Self::InvalidResponse => "invalid_response",
            Self::NoMatch => "no_match",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn retry_delay_seconds(self, attempts: i64) -> Option<i64> {
        let base_seconds: i64 = match self {
            Self::Network => 60,
            Self::RateLimited => 15 * 60,
            Self::ProviderUnavailable => 5 * 60,
            Self::WorkerInterrupted => 60,
            Self::InvalidResponse | Self::NoMatch | Self::Unsupported => return None,
        };
        let exponent = u32::try_from((attempts - 1).clamp(0, 6)).unwrap_or(6);
        Some(
            base_seconds
                .saturating_mul(1_i64 << exponent)
                .min(24 * 60 * 60),
        )
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "network" => Ok(Self::Network),
            "rate_limited" => Ok(Self::RateLimited),
            "provider_unavailable" => Ok(Self::ProviderUnavailable),
            "worker_interrupted" => Ok(Self::WorkerInterrupted),
            "invalid_response" => Ok(Self::InvalidResponse),
            "no_match" => Ok(Self::NoMatch),
            "unsupported" => Ok(Self::Unsupported),
            _ => Err(format!("未知 external search error kind：{value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSearchCompletionStatus {
    Succeeded,
    Partial,
}

impl ExternalSearchCompletionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Partial => "partial",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchJobIssue {
    pub field: Option<MetadataField>,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSearchJobSummary {
    pub candidates_received: usize,
    pub tags_received: usize,
    pub tags_applied: usize,
    pub auto_applied: usize,
    pub suggestions: usize,
    pub search_only: usize,
    pub issues: Vec<ExternalSearchJobIssue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSearchJobSnapshot {
    pub id: i64,
    pub collection_id: i64,
    pub status: ExternalSearchJobStatus,
    pub fields: Vec<MetadataField>,
    pub result_json: Option<String>,
    pub error_kind: Option<ExternalSearchErrorKind>,
    pub error_message: Option<String>,
    pub attempts: i64,
    pub next_retry_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSearchEnqueueOutcome {
    pub job: ExternalSearchJobSnapshot,
    pub created: bool,
}

impl CatalogRepository {
    pub fn enqueue_external_search(
        &mut self,
        collection_id: i64,
        fields: &[MetadataField],
    ) -> StorageResult<ExternalSearchEnqueueOutcome> {
        let fields = normalized_fields(fields)?;
        let payload_json = serde_json::json!({
            "fields": fields.iter().map(|field| field.as_str()).collect::<Vec<_>>()
        })
        .to_string();
        let transaction = self.connection.transaction()?;
        super::ensure_collection(&transaction, collection_id)?;
        let existing_id = transaction
            .query_row(
                "SELECT id FROM background_jobs
                 WHERE collection_id = ?1 AND job_kind = 'external_search'
                   AND status IN ('pending', 'running')
                 ORDER BY id DESC LIMIT 1",
                [collection_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(job_id) = existing_id {
            transaction.commit()?;
            return Ok(ExternalSearchEnqueueOutcome {
                job: self.external_search_job(job_id)?,
                created: false,
            });
        }
        transaction.execute(
            "INSERT INTO background_jobs(collection_id, job_kind, status, payload_json)
             VALUES (?1, 'external_search', 'pending', ?2)",
            params![collection_id, payload_json],
        )?;
        let job_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(ExternalSearchEnqueueOutcome {
            job: self.external_search_job(job_id)?,
            created: true,
        })
    }

    pub fn external_search_job(&self, job_id: i64) -> StorageResult<ExternalSearchJobSnapshot> {
        let sql = format!(
            "SELECT {EXTERNAL_SEARCH_JOB_COLUMNS} FROM background_jobs
             WHERE id = ?1 AND job_kind = 'external_search'"
        );
        let raw = self
            .connection
            .query_row(&sql, [job_id], raw_external_search_job)
            .optional()?
            .ok_or(StorageError::ExternalSearchJobNotFound(job_id))?;
        raw.try_into()
    }

    pub fn due_external_search_jobs(
        &self,
        limit: u32,
    ) -> StorageResult<Vec<ExternalSearchJobSnapshot>> {
        let limit = limit.clamp(1, 200);
        let sql = format!(
            "SELECT {EXTERNAL_SEARCH_JOB_COLUMNS} FROM background_jobs
             WHERE job_kind = 'external_search' AND status = 'pending'
               AND (next_retry_at IS NULL OR next_retry_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             ORDER BY COALESCE(next_retry_at, created_at), id LIMIT ?1"
        );
        let mut statement = self.connection.prepare(&sql)?;
        statement
            .query_map([limit], raw_external_search_job)?
            .map(|row| row.map_err(StorageError::from).and_then(TryInto::try_into))
            .collect()
    }

    pub fn start_external_search_job(
        &mut self,
        job_id: i64,
    ) -> StorageResult<ExternalSearchJobSnapshot> {
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE background_jobs
             SET status = 'running', attempts = attempts + 1, next_retry_at = NULL,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1 AND job_kind = 'external_search' AND status = 'pending'
               AND (next_retry_at IS NULL OR next_retry_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            [job_id],
        )?;
        if changed == 0 {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM background_jobs WHERE id = ?1 AND job_kind = 'external_search'
                 )",
                [job_id],
                |row| row.get(0),
            )?;
            return Err(if exists {
                StorageError::ExternalSearchJobUnavailable(job_id)
            } else {
                StorageError::ExternalSearchJobNotFound(job_id)
            });
        }
        transaction.commit()?;
        self.external_search_job(job_id)
    }

    pub fn recover_interrupted_external_search_jobs(&mut self) -> StorageResult<usize> {
        let changed = self.connection.execute(
            "UPDATE background_jobs
             SET status = 'pending', error_kind = 'worker_interrupted',
                 error_message = '先前程序在工作完成前停止', next_retry_at = NULL,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE job_kind = 'external_search' AND status = 'running'",
            [],
        )?;
        Ok(changed)
    }

    pub fn requeue_interrupted_external_search_job(
        &mut self,
        job_id: i64,
        message: &str,
    ) -> StorageResult<ExternalSearchJobSnapshot> {
        let message = message.trim();
        if message.is_empty() {
            return Err(StorageError::InvalidExternalSearchJob(
                "中斷工作必須包含錯誤訊息".to_owned(),
            ));
        }
        let changed = self.connection.execute(
            "UPDATE background_jobs
             SET status = 'pending', error_kind = 'worker_interrupted', error_message = ?1,
                 next_retry_at = NULL,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?2 AND job_kind = 'external_search' AND status = 'running'",
            params![message, job_id],
        )?;
        if changed == 0 {
            return Err(StorageError::ExternalSearchJobUnavailable(job_id));
        }
        self.external_search_job(job_id)
    }

    pub fn complete_external_search_job(
        &mut self,
        job_id: i64,
        status: ExternalSearchCompletionStatus,
        summary: &ExternalSearchJobSummary,
    ) -> StorageResult<ExternalSearchJobSnapshot> {
        let result_json = encode_summary(summary)?;
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE background_jobs
             SET status = ?1, result_json = ?2, error_kind = NULL, error_message = NULL,
                 next_retry_at = NULL,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?3 AND job_kind = 'external_search' AND status = 'running'",
            params![status.as_str(), result_json, job_id],
        )?;
        if changed == 0 {
            return Err(StorageError::ExternalSearchJobUnavailable(job_id));
        }
        transaction.commit()?;
        self.external_search_job(job_id)
    }

    pub fn fail_external_search_job(
        &mut self,
        job_id: i64,
        error_kind: ExternalSearchErrorKind,
        error_message: &str,
        summary: Option<&ExternalSearchJobSummary>,
    ) -> StorageResult<ExternalSearchJobSnapshot> {
        let error_message = error_message.trim();
        if error_message.is_empty() {
            return Err(StorageError::InvalidExternalSearchJob(
                "失敗工作必須包含錯誤訊息".to_owned(),
            ));
        }
        let result_json = summary.map(encode_summary).transpose()?;
        let transaction = self.connection.transaction()?;
        let attempts = transaction
            .query_row(
                "SELECT attempts FROM background_jobs
                 WHERE id = ?1 AND job_kind = 'external_search' AND status = 'running'",
                [job_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(StorageError::ExternalSearchJobUnavailable(job_id))?;
        if let Some(delay_seconds) = error_kind.retry_delay_seconds(attempts) {
            let modifier = format!("+{delay_seconds} seconds");
            transaction.execute(
                "UPDATE background_jobs
                 SET status = 'pending', result_json = ?1, error_kind = ?2, error_message = ?3,
                     next_retry_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?4),
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ?5",
                params![
                    result_json,
                    error_kind.as_str(),
                    error_message,
                    modifier,
                    job_id
                ],
            )?;
        } else {
            transaction.execute(
                "UPDATE background_jobs
                 SET status = 'failed', result_json = ?1, error_kind = ?2, error_message = ?3,
                     next_retry_at = NULL,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ?4",
                params![result_json, error_kind.as_str(), error_message, job_id],
            )?;
        }
        transaction.commit()?;
        self.external_search_job(job_id)
    }
}

fn normalized_fields(fields: &[MetadataField]) -> StorageResult<Vec<MetadataField>> {
    let normalized = MetadataField::ALL
        .into_iter()
        .filter(|field| fields.contains(field))
        .collect::<Vec<_>>();
    if normalized.is_empty() {
        return Err(StorageError::InvalidExternalSearchJob(
            "至少必須指定一個 metadata field".to_owned(),
        ));
    }
    Ok(normalized)
}

fn encode_summary(summary: &ExternalSearchJobSummary) -> StorageResult<String> {
    let mut issues = Vec::with_capacity(summary.issues.len());
    for issue in &summary.issues {
        if issue.kind.trim().is_empty() || issue.message.trim().is_empty() {
            return Err(StorageError::InvalidExternalSearchJob(
                "工作 issue 必須包含 kind 與 message".to_owned(),
            ));
        }
        issues.push(serde_json::json!({
            "field": issue.field.map(MetadataField::as_str),
            "kind": issue.kind,
            "message": issue.message,
        }));
    }
    Ok(serde_json::json!({
        "candidates_received": summary.candidates_received,
        "tags_received": summary.tags_received,
        "tags_applied": summary.tags_applied,
        "auto_applied": summary.auto_applied,
        "suggestions": summary.suggestions,
        "search_only": summary.search_only,
        "issues": issues,
    })
    .to_string())
}

struct RawExternalSearchJob {
    id: i64,
    collection_id: Option<i64>,
    status: String,
    payload_json: String,
    result_json: Option<String>,
    error_kind: Option<String>,
    error_message: Option<String>,
    attempts: i64,
    next_retry_at: Option<String>,
    created_at: String,
    updated_at: String,
}

fn raw_external_search_job(row: &Row<'_>) -> rusqlite::Result<RawExternalSearchJob> {
    Ok(RawExternalSearchJob {
        id: row.get(0)?,
        collection_id: row.get(1)?,
        status: row.get(2)?,
        payload_json: row.get(3)?,
        result_json: row.get(4)?,
        error_kind: row.get(5)?,
        error_message: row.get(6)?,
        attempts: row.get(7)?,
        next_retry_at: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

impl TryFrom<RawExternalSearchJob> for ExternalSearchJobSnapshot {
    type Error = StorageError;

    fn try_from(raw: RawExternalSearchJob) -> Result<Self, Self::Error> {
        let payload: serde_json::Value = serde_json::from_str(&raw.payload_json)?;
        let field_values = payload
            .get("fields")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                StorageError::InvalidSchema(
                    "external search job payload 缺少 fields array".to_owned(),
                )
            })?;
        let mut fields = Vec::with_capacity(field_values.len());
        for value in field_values {
            let value = value.as_str().ok_or_else(|| {
                StorageError::InvalidSchema("external search job field 必須是 string".to_owned())
            })?;
            let field = MetadataField::parse(value).map_err(StorageError::InvalidSchema)?;
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
        if fields.is_empty() {
            return Err(StorageError::InvalidSchema(
                "external search job 至少必須有一個 field".to_owned(),
            ));
        }
        Ok(Self {
            id: raw.id,
            collection_id: raw.collection_id.ok_or_else(|| {
                StorageError::InvalidSchema("external search job 必須關聯 collection".to_owned())
            })?,
            status: ExternalSearchJobStatus::parse(&raw.status)
                .map_err(StorageError::InvalidSchema)?,
            fields,
            result_json: raw.result_json,
            error_kind: raw
                .error_kind
                .as_deref()
                .map(ExternalSearchErrorKind::parse)
                .transpose()
                .map_err(StorageError::InvalidSchema)?,
            error_message: raw.error_message,
            attempts: raw.attempts,
            next_retry_at: raw.next_retry_at,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
        })
    }
}
