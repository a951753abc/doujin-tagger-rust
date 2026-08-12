//! Persistent scan-run journal and issue records.

use std::path::PathBuf;

use doujin_scanner::{ScanRoot, SourceKind};
use rusqlite::{OptionalExtension, params};

use crate::{CatalogRepository, StorageError, StorageResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanCompletionStatus {
    Succeeded,
    Partial,
    Failed,
}

impl ScanCompletionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanRunStatus {
    Running,
    Succeeded,
    Partial,
    Failed,
}

impl ScanRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "partial" => Ok(Self::Partial),
            "failed" => Ok(Self::Failed),
            _ => Err(StorageError::InvalidSchema(format!(
                "未知 scan run status：{value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanIssueRecord {
    pub path: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCompletion {
    pub status: ScanCompletionStatus,
    pub summary_json: String,
    pub issues: Vec<ScanIssueRecord>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRunSnapshot {
    pub id: i64,
    pub status: ScanRunStatus,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub summary_json: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanIssueSnapshot {
    pub id: i64,
    pub path: String,
    pub kind: String,
    pub message: String,
}

impl CatalogRepository {
    pub fn active_scan_roots(&self) -> StorageResult<Vec<ScanRoot>> {
        let mut statement = self.connection.prepare(
            "SELECT path, source_kind, label
             FROM library_roots WHERE active = 1 ORDER BY id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(path, source, label)| {
                let source = match source.as_str() {
                    "archive" => SourceKind::Archive,
                    "downloads" => SourceKind::Downloads,
                    _ => {
                        return Err(StorageError::InvalidSchema(format!(
                            "未知 library root source：{source}"
                        )));
                    }
                };
                Ok(ScanRoot {
                    path: PathBuf::from(path),
                    source,
                    label,
                })
            })
            .collect()
    }

    pub fn begin_scan_run(&mut self) -> StorageResult<i64> {
        let transaction = self.connection.transaction()?;
        let running: Option<i64> = transaction
            .query_row(
                "SELECT id FROM scan_runs WHERE status = 'running' ORDER BY id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if running.is_some() {
            return Err(StorageError::ScanAlreadyRunning);
        }
        transaction.execute("INSERT INTO scan_runs(status) VALUES ('running')", [])?;
        let scan_run_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(scan_run_id)
    }

    pub fn complete_scan_run(
        &mut self,
        scan_run_id: i64,
        completion: ScanCompletion,
    ) -> StorageResult<()> {
        validate_completion(&completion)?;
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE scan_runs
             SET status = ?1,
                 completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                 summary_json = ?2,
                 error_message = ?3
             WHERE id = ?4 AND status = 'running'",
            params![
                completion.status.as_str(),
                completion.summary_json,
                completion.error_message,
                scan_run_id,
            ],
        )?;
        if changed == 0 {
            let existing_status: Option<String> = transaction
                .query_row(
                    "SELECT status FROM scan_runs WHERE id = ?1",
                    [scan_run_id],
                    |row| row.get(0),
                )
                .optional()?;
            return match existing_status {
                None => Err(StorageError::ScanRunNotFound(scan_run_id)),
                Some(status) => Err(StorageError::InvalidScanRun(format!(
                    "scan run {scan_run_id} 已是 {status}，不能再次完成"
                ))),
            };
        }
        for issue in completion.issues {
            transaction.execute(
                "INSERT INTO scan_issues(scan_run_id, path, issue_kind, message)
                 VALUES (?1, ?2, ?3, ?4)",
                params![scan_run_id, issue.path, issue.kind, issue.message],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn scan_run(&self, scan_run_id: i64) -> StorageResult<ScanRunSnapshot> {
        let row = self
            .connection
            .query_row(
                "SELECT id, status, started_at, completed_at, summary_json, error_message
                 FROM scan_runs WHERE id = ?1",
                [scan_run_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StorageError::ScanRunNotFound(scan_run_id))?;
        Ok(ScanRunSnapshot {
            id: row.0,
            status: ScanRunStatus::parse(&row.1)?,
            started_at: row.2,
            completed_at: row.3,
            summary_json: row.4,
            error_message: row.5,
        })
    }

    pub fn scan_issues(&self, scan_run_id: i64) -> StorageResult<Vec<ScanIssueSnapshot>> {
        self.scan_run(scan_run_id)?;
        let mut statement = self.connection.prepare(
            "SELECT id, path, issue_kind, message
             FROM scan_issues WHERE scan_run_id = ?1 ORDER BY id",
        )?;
        Ok(statement
            .query_map([scan_run_id], |row| {
                Ok(ScanIssueSnapshot {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    kind: row.get(2)?,
                    message: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn scan_run_count(&self) -> StorageResult<i64> {
        Ok(self
            .connection
            .query_row("SELECT count(*) FROM scan_runs", [], |row| row.get(0))?)
    }
}

fn validate_completion(completion: &ScanCompletion) -> StorageResult<()> {
    let summary: serde_json::Value = serde_json::from_str(&completion.summary_json)
        .map_err(|error| StorageError::InvalidScanRun(format!("summary JSON 無效：{error}")))?;
    if !summary.is_object() {
        return Err(StorageError::InvalidScanRun(
            "summary JSON 必須是 object".to_owned(),
        ));
    }
    let error_present = completion
        .error_message
        .as_deref()
        .is_some_and(|message| !message.trim().is_empty());
    match completion.status {
        ScanCompletionStatus::Succeeded
            if !completion.issues.is_empty() || completion.error_message.is_some() =>
        {
            return Err(StorageError::InvalidScanRun(
                "succeeded scan 不得包含 issue 或 error message".to_owned(),
            ));
        }
        ScanCompletionStatus::Partial if completion.issues.is_empty() => {
            return Err(StorageError::InvalidScanRun(
                "partial scan 必須包含至少一個 issue".to_owned(),
            ));
        }
        ScanCompletionStatus::Failed if !error_present => {
            return Err(StorageError::InvalidScanRun(
                "failed scan 必須包含 error message".to_owned(),
            ));
        }
        _ => {}
    }
    for issue in &completion.issues {
        if issue.kind.trim().is_empty() || issue.message.trim().is_empty() {
            return Err(StorageError::InvalidScanRun(
                "scan issue 的 kind 與 message 不得為空白".to_owned(),
            ));
        }
    }
    Ok(())
}
