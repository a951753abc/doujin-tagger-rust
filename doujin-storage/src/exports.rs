//! Persistent export destinations and ZIP package jobs.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};

use crate::{CatalogRepository, StorageError, StorageResult, path_key, path_text};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRootSnapshot {
    pub id: i64,
    pub path: PathBuf,
    pub label: String,
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportJobStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportJobItemStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl ExportJobItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => Err(StorageError::InvalidSchema(format!(
                "未知 export job item status：{value}"
            ))),
        }
    }
}

impl ExportJobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => Err(StorageError::InvalidSchema(format!(
                "未知 export job status：{value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExportJobItem {
    pub collection_id: i64,
    pub package_entry: String,
    pub original_filename: String,
    pub expected_source_identity: String,
    pub source_size: u64,
    pub manifest_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportJobSnapshot {
    pub id: i64,
    pub export_root_id: i64,
    pub package_filename: String,
    pub status: ExportJobStatus,
    pub total_items: usize,
    pub processed_items: usize,
    pub total_bytes: u64,
    pub processed_bytes: u64,
    pub current_collection_id: Option<i64>,
    pub succeeded_items: usize,
    pub failed_items: usize,
    pub attempts: usize,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportJobItemSnapshot {
    pub job_id: i64,
    pub collection_id: i64,
    pub package_entry: String,
    pub original_filename: String,
    pub expected_source_identity: String,
    pub source_size: u64,
    pub manifest_json: String,
    pub status: ExportJobItemStatus,
    pub bytes_copied: u64,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

impl CatalogRepository {
    pub fn export_roots(&self) -> StorageResult<Vec<ExportRootSnapshot>> {
        let mut statement = self.connection.prepare(
            "SELECT id, path, label, active, created_at, updated_at
             FROM export_roots ORDER BY id",
        )?;
        statement
            .query_map([], map_export_root)?
            .map(|row| row.map_err(StorageError::from).and_then(decode_export_root))
            .collect()
    }

    pub fn export_root(&self, root_id: i64) -> StorageResult<ExportRootSnapshot> {
        let row = self
            .connection
            .query_row(
                "SELECT id, path, label, active, created_at, updated_at
                 FROM export_roots WHERE id = ?1",
                [root_id],
                map_export_root,
            )
            .optional()?
            .ok_or(StorageError::ExportRootNotFound(root_id))?;
        decode_export_root(row)
    }

    pub fn register_export_root(
        &mut self,
        path: &Path,
        label: &str,
    ) -> StorageResult<ExportRootSnapshot> {
        let path = validated_export_root(path, label)?;
        self.ensure_export_root_path_available(None, &path)?;
        self.connection.execute(
            "INSERT INTO export_roots(path, path_key, label) VALUES (?1, ?2, ?3)",
            params![path_text(&path)?, path_key(&path), label.trim()],
        )?;
        self.export_root(self.connection.last_insert_rowid())
    }

    pub fn update_export_root(
        &mut self,
        root_id: i64,
        path: &Path,
        label: &str,
    ) -> StorageResult<ExportRootSnapshot> {
        self.export_root(root_id)?;
        let path = validated_export_root(path, label)?;
        self.ensure_export_root_path_available(Some(root_id), &path)?;
        let changed = self.connection.execute(
            "UPDATE export_roots
             SET path = ?1, path_key = ?2, label = ?3,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?4",
            params![path_text(&path)?, path_key(&path), label.trim(), root_id],
        )?;
        if changed == 0 {
            return Err(StorageError::ExportRootNotFound(root_id));
        }
        self.export_root(root_id)
    }

    pub fn deactivate_export_root(&mut self, root_id: i64) -> StorageResult<ExportRootSnapshot> {
        self.set_export_root_active(root_id, false)
    }

    pub fn reactivate_export_root(&mut self, root_id: i64) -> StorageResult<ExportRootSnapshot> {
        let root = self.export_root(root_id)?;
        validated_export_root(&root.path, &root.label)?;
        self.set_export_root_active(root_id, true)
    }

    pub fn create_export_job(
        &mut self,
        export_root_id: i64,
        package_filename: &str,
        items: &[NewExportJobItem],
    ) -> StorageResult<ExportJobSnapshot> {
        let root = self.export_root(export_root_id)?;
        if !root.active {
            return Err(StorageError::InvalidExportJob(
                "匯出目的地已停用".to_owned(),
            ));
        }
        let package_filename = package_filename.trim();
        if package_filename.is_empty() || items.is_empty() {
            return Err(StorageError::InvalidExportJob(
                "匯出工作必須包含 package 檔名與至少一筆收藏".to_owned(),
            ));
        }
        let total_bytes = items.iter().try_fold(0_u64, |total, item| {
            total.checked_add(item.source_size).ok_or_else(|| {
                StorageError::InvalidExportJob("匯出來源總大小超出支援範圍".to_owned())
            })
        })?;
        let total_items = i64::try_from(items.len())
            .map_err(|_| StorageError::InvalidExportJob("匯出收藏數量超出支援範圍".to_owned()))?;
        let total_bytes = i64::try_from(total_bytes)
            .map_err(|_| StorageError::InvalidExportJob("匯出來源總大小超出支援範圍".to_owned()))?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO export_jobs(export_root_id, package_filename, total_items, total_bytes)
             VALUES (?1, ?2, ?3, ?4)",
            params![export_root_id, package_filename, total_items, total_bytes],
        )?;
        let job_id = transaction.last_insert_rowid();
        for item in items {
            super::ensure_collection(&transaction, item.collection_id)?;
            serde_json::from_str::<serde_json::Value>(&item.manifest_json)?;
            let source_size = i64::try_from(item.source_size).map_err(|_| {
                StorageError::InvalidExportJob(format!(
                    "收藏 {} 的來源大小超出支援範圍",
                    item.collection_id
                ))
            })?;
            transaction.execute(
                "INSERT INTO export_job_items(
                     job_id, collection_id, package_entry, original_filename,
                     expected_source_identity, source_size, manifest_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    job_id,
                    item.collection_id,
                    item.package_entry,
                    item.original_filename,
                    item.expected_source_identity,
                    source_size,
                    item.manifest_json,
                ],
            )?;
        }
        transaction.commit()?;
        self.export_job(job_id)
    }

    pub fn export_job(&self, job_id: i64) -> StorageResult<ExportJobSnapshot> {
        let row = self
            .connection
            .query_row(
                "SELECT id, export_root_id, package_filename, status, total_items,
                        processed_items, total_bytes, processed_bytes, current_collection_id,
                        succeeded_items, failed_items, attempts, error_message, created_at,
                        updated_at, completed_at
                 FROM export_jobs WHERE id = ?1",
                [job_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, Option<String>>(15)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StorageError::ExportJobNotFound(job_id))?;
        Ok(ExportJobSnapshot {
            id: row.0,
            export_root_id: row.1,
            package_filename: row.2,
            status: ExportJobStatus::parse(&row.3)?,
            total_items: nonnegative_usize(row.4, "total_items")?,
            processed_items: nonnegative_usize(row.5, "processed_items")?,
            total_bytes: nonnegative_u64(row.6, "total_bytes")?,
            processed_bytes: nonnegative_u64(row.7, "processed_bytes")?,
            current_collection_id: row.8,
            succeeded_items: nonnegative_usize(row.9, "succeeded_items")?,
            failed_items: nonnegative_usize(row.10, "failed_items")?,
            attempts: nonnegative_usize(row.11, "attempts")?,
            error_message: row.12,
            created_at: row.13,
            updated_at: row.14,
            completed_at: row.15,
        })
    }

    pub fn latest_export_job(&self) -> StorageResult<Option<ExportJobSnapshot>> {
        let job_id = self
            .connection
            .query_row(
                "SELECT id FROM export_jobs ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        job_id.map(|job_id| self.export_job(job_id)).transpose()
    }

    pub fn export_job_items(&self, job_id: i64) -> StorageResult<Vec<ExportJobItemSnapshot>> {
        self.export_job(job_id)?;
        let mut statement = self.connection.prepare(
            "SELECT job_id, collection_id, package_entry, original_filename,
                    expected_source_identity, source_size, manifest_json, status, bytes_copied,
                    error_message, started_at, completed_at
             FROM export_job_items WHERE job_id = ?1 ORDER BY collection_id",
        )?;
        statement
            .query_map([job_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                ))
            })?
            .map(|row| {
                let row = row?;
                Ok(ExportJobItemSnapshot {
                    job_id: row.0,
                    collection_id: row.1,
                    package_entry: row.2,
                    original_filename: row.3,
                    expected_source_identity: row.4,
                    source_size: nonnegative_u64(row.5, "item source_size")?,
                    manifest_json: row.6,
                    status: ExportJobItemStatus::parse(&row.7)?,
                    bytes_copied: nonnegative_u64(row.8, "item bytes_copied")?,
                    error_message: row.9,
                    started_at: row.10,
                    completed_at: row.11,
                })
            })
            .collect()
    }

    pub fn start_export_job(&mut self, job_id: i64) -> StorageResult<ExportJobSnapshot> {
        let changed = self.connection.execute(
            "UPDATE export_jobs
             SET status = 'running', attempts = attempts + 1,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1 AND status = 'pending'",
            [job_id],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidExportJob(format!(
                "export job {job_id} 目前不可開始"
            )));
        }
        self.export_job(job_id)
    }

    pub fn start_export_job_item(
        &mut self,
        job_id: i64,
        collection_id: i64,
    ) -> StorageResult<ExportJobItemSnapshot> {
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE export_job_items
             SET status = 'running', started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE job_id = ?1 AND collection_id = ?2 AND status = 'pending'
               AND EXISTS (SELECT 1 FROM export_jobs WHERE id = ?1 AND status = 'running')",
            params![job_id, collection_id],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidExportJob(format!(
                "export job {job_id} 的收藏 {collection_id} 目前不可開始"
            )));
        }
        transaction.execute(
            "UPDATE export_jobs SET current_collection_id = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![job_id, collection_id],
        )?;
        transaction.commit()?;
        self.export_job_item(job_id, collection_id)
    }

    pub fn update_export_item_progress(
        &mut self,
        job_id: i64,
        collection_id: i64,
        bytes_copied: u64,
    ) -> StorageResult<ExportJobSnapshot> {
        let bytes_copied = i64::try_from(bytes_copied)
            .map_err(|_| StorageError::InvalidExportJob("匯出進度超出支援範圍".to_owned()))?;
        let transaction = self.connection.transaction()?;
        let previous = transaction
            .query_row(
                "SELECT bytes_copied FROM export_job_items
                 WHERE job_id = ?1 AND collection_id = ?2 AND status = 'running'",
                params![job_id, collection_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::InvalidExportJob(format!(
                    "export job {job_id} 的收藏 {collection_id} 沒有執行中的 item"
                ))
            })?;
        if bytes_copied < previous {
            return Err(StorageError::InvalidExportJob(
                "匯出 bytes 進度不得倒退".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE export_job_items SET bytes_copied = ?3
             WHERE job_id = ?1 AND collection_id = ?2",
            params![job_id, collection_id, bytes_copied],
        )?;
        transaction.execute(
            "UPDATE export_jobs SET processed_bytes = processed_bytes + (?3 - ?2),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![job_id, previous, bytes_copied],
        )?;
        transaction.commit()?;
        self.export_job(job_id)
    }

    pub fn complete_export_job_item(
        &mut self,
        job_id: i64,
        collection_id: i64,
    ) -> StorageResult<ExportJobSnapshot> {
        let transaction = self.connection.transaction()?;
        let (source_size, bytes_copied) = transaction
            .query_row(
                "SELECT source_size, bytes_copied FROM export_job_items
                 WHERE job_id = ?1 AND collection_id = ?2 AND status = 'running'",
                params![job_id, collection_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::InvalidExportJob(format!(
                    "export job {job_id} 的收藏 {collection_id} 目前不可完成"
                ))
            })?;
        transaction.execute(
            "UPDATE export_job_items SET status = 'succeeded', bytes_copied = source_size,
                 completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE job_id = ?1 AND collection_id = ?2",
            params![job_id, collection_id],
        )?;
        transaction.execute(
            "UPDATE export_jobs
             SET processed_items = processed_items + 1, succeeded_items = succeeded_items + 1,
                 processed_bytes = processed_bytes + (?3 - ?2), current_collection_id = NULL,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![job_id, bytes_copied, source_size],
        )?;
        transaction.commit()?;
        self.export_job(job_id)
    }

    pub fn complete_export_job(&mut self, job_id: i64) -> StorageResult<ExportJobSnapshot> {
        let changed = self.connection.execute(
            "UPDATE export_jobs SET status = 'succeeded', current_collection_id = NULL,
                 completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1 AND status = 'running'
               AND processed_items = total_items AND succeeded_items = total_items
               AND failed_items = 0",
            [job_id],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidExportJob(format!(
                "export job {job_id} 尚未全部成功"
            )));
        }
        self.export_job(job_id)
    }

    pub fn fail_export_job(
        &mut self,
        job_id: i64,
        collection_id: Option<i64>,
        message: &str,
    ) -> StorageResult<ExportJobSnapshot> {
        let message = message.trim();
        if message.is_empty() {
            return Err(StorageError::InvalidExportJob(
                "失敗的 export job 必須包含原因".to_owned(),
            ));
        }
        let transaction = self.connection.transaction()?;
        if let Some(collection_id) = collection_id {
            transaction.execute(
                "UPDATE export_job_items SET status = 'failed', error_message = ?3,
                     completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE job_id = ?1 AND collection_id = ?2 AND status IN ('pending', 'running')",
                params![job_id, collection_id, message],
            )?;
        }
        let changed = transaction.execute(
            "UPDATE export_jobs SET status = 'failed', error_message = ?2,
                 failed_items = CASE WHEN ?3 IS NULL THEN failed_items ELSE failed_items + 1 END,
                 current_collection_id = NULL,
                 completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1 AND status IN ('pending', 'running')",
            params![job_id, message, collection_id],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidExportJob(format!(
                "export job {job_id} 目前不可標記失敗"
            )));
        }
        transaction.commit()?;
        self.export_job(job_id)
    }

    pub fn recover_interrupted_export_jobs(&mut self) -> StorageResult<usize> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE export_job_items SET status = 'failed', error_message = '先前程序在匯出完成前停止',
                 completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE job_id IN (SELECT id FROM export_jobs WHERE status = 'running')
               AND status = 'running'",
            [],
        )?;
        let changed = transaction.execute(
            "UPDATE export_jobs SET status = 'failed', error_message = '先前程序在匯出完成前停止',
                 failed_items = failed_items + CASE WHEN current_collection_id IS NULL THEN 0 ELSE 1 END,
                 current_collection_id = NULL,
                 completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE status = 'running'",
            [],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn retry_export_job(&mut self, job_id: i64) -> StorageResult<ExportJobSnapshot> {
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE export_jobs SET status = 'pending', processed_items = 0, processed_bytes = 0,
                 current_collection_id = NULL, succeeded_items = 0, failed_items = 0,
                 error_message = NULL, completed_at = NULL,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1 AND status = 'failed'",
            [job_id],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidExportJob(format!(
                "export job {job_id} 目前不可重試"
            )));
        }
        transaction.execute(
            "UPDATE export_job_items SET status = 'pending', bytes_copied = 0,
                 error_message = NULL, started_at = NULL, completed_at = NULL WHERE job_id = ?1",
            [job_id],
        )?;
        transaction.commit()?;
        self.export_job(job_id)
    }

    fn set_export_root_active(
        &mut self,
        root_id: i64,
        active: bool,
    ) -> StorageResult<ExportRootSnapshot> {
        let changed = self.connection.execute(
            "UPDATE export_roots SET active = ?1,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?2",
            params![active, root_id],
        )?;
        if changed == 0 {
            return Err(StorageError::ExportRootNotFound(root_id));
        }
        self.export_root(root_id)
    }

    fn export_job_item(
        &self,
        job_id: i64,
        collection_id: i64,
    ) -> StorageResult<ExportJobItemSnapshot> {
        self.export_job_items(job_id)?
            .into_iter()
            .find(|item| item.collection_id == collection_id)
            .ok_or_else(|| {
                StorageError::InvalidExportJob(format!(
                    "export job {job_id} 找不到收藏 {collection_id}"
                ))
            })
    }

    fn ensure_export_root_path_available(
        &self,
        except_id: Option<i64>,
        path: &Path,
    ) -> StorageResult<()> {
        let conflict = self
            .connection
            .query_row(
                "SELECT id FROM export_roots WHERE path_key = ?1 AND id <> COALESCE(?2, -1)",
                params![path_key(path), except_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(conflict) = conflict {
            return Err(StorageError::InvalidExportRoot(format!(
                "此路徑已由匯出目的地 {conflict} 使用"
            )));
        }
        Ok(())
    }
}

type ExportRootRow = (i64, String, String, bool, String, String);

fn map_export_root(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExportRootRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn decode_export_root(row: ExportRootRow) -> StorageResult<ExportRootSnapshot> {
    Ok(ExportRootSnapshot {
        id: row.0,
        path: PathBuf::from(row.1),
        label: row.2,
        active: row.3,
        created_at: row.4,
        updated_at: row.5,
    })
}

fn validated_export_root(path: &Path, label: &str) -> StorageResult<PathBuf> {
    if !path.is_absolute() || label.trim().is_empty() {
        return Err(StorageError::InvalidExportRoot(
            "匯出目的地必須是絕對路徑，且 label 不得為空白".to_owned(),
        ));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StorageError::InvalidExportRoot(format!("匯出目的地無法存取：{error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StorageError::InvalidExportRoot(
            "匯出目的地必須是一般資料夾且不能是 symlink".to_owned(),
        ));
    }
    fs::canonicalize(path).map_err(|error| {
        StorageError::InvalidExportRoot(format!("匯出目的地無法 canonicalize：{error}"))
    })
}

fn nonnegative_usize(value: i64, field: &str) -> StorageResult<usize> {
    usize::try_from(value)
        .map_err(|_| StorageError::InvalidSchema(format!("export {field} 無效：{value}")))
}

fn nonnegative_u64(value: i64, field: &str) -> StorageResult<u64> {
    u64::try_from(value)
        .map_err(|_| StorageError::InvalidSchema(format!("export {field} 無效：{value}")))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TestTree(PathBuf);

    impl TestTree {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "doujin-export-storage-{label}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("test tree");
            Self(path)
        }

        fn directory(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(&path).expect("test directory");
            path
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            if self
                .0
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("doujin-export-storage-"))
            {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }

    fn insert_collections(repository: &CatalogRepository, count: i64) {
        for _ in 0..count {
            repository
                .connection
                .execute("INSERT INTO collections DEFAULT VALUES", [])
                .expect("insert collection");
        }
    }

    fn items(count: i64, bytes: u64) -> Vec<NewExportJobItem> {
        (1..=count)
            .map(|collection_id| NewExportJobItem {
                collection_id,
                package_entry: format!("book-{collection_id}.zip"),
                original_filename: format!("book-{collection_id}.zip"),
                expected_source_identity: format!("{bytes}:123456789"),
                source_size: bytes,
                manifest_json: serde_json::json!({
                    "collection_id": collection_id,
                    "package_entry": format!("book-{collection_id}.zip")
                })
                .to_string(),
            })
            .collect()
    }

    #[test]
    fn export_root_crud_canonicalizes_and_rejects_duplicate_paths() {
        let tree = TestTree::new("roots");
        let destination = tree.directory("exports");
        let mut repository = CatalogRepository::open_in_memory().expect("catalog");
        let created = repository
            .register_export_root(&destination.join("."), "外接硬碟")
            .expect("register export root");
        assert_eq!(
            fs::canonicalize(&destination).expect("canonical"),
            created.path
        );
        assert!(created.active);
        assert!(matches!(
            repository.register_export_root(&destination, "重複"),
            Err(StorageError::InvalidExportRoot(_))
        ));

        let second = tree.directory("second");
        let updated = repository
            .update_export_root(created.id, &second, "第二目的地")
            .expect("update root");
        assert_eq!("第二目的地", updated.label);
        assert!(
            !repository
                .deactivate_export_root(created.id)
                .expect("disable")
                .active
        );
        assert!(
            repository
                .reactivate_export_root(created.id)
                .expect("enable")
                .active
        );
        assert_eq!(1, repository.export_roots().expect("roots").len());
    }

    #[test]
    fn export_root_rejects_relative_missing_file_and_blank_label() {
        let tree = TestTree::new("invalid-roots");
        let file = tree.0.join("not-a-directory");
        fs::write(&file, b"x").expect("file");
        let mut repository = CatalogRepository::open_in_memory().expect("catalog");
        for (path, label) in [
            (PathBuf::from("relative"), "relative"),
            (tree.0.join("missing"), "missing"),
            (file, "file"),
            (tree.0.clone(), "  "),
        ] {
            assert!(matches!(
                repository.register_export_root(&path, label),
                Err(StorageError::InvalidExportRoot(_))
            ));
        }
    }

    #[cfg(windows)]
    #[test]
    fn export_root_rejects_directory_symlink_when_creation_is_permitted() {
        use std::os::windows::fs::symlink_dir;

        let tree = TestTree::new("symlink-root");
        let target = tree.directory("target");
        let link = tree.0.join("link");
        if symlink_dir(target, &link).is_err() {
            return;
        }
        let mut repository = CatalogRepository::open_in_memory().expect("catalog");
        assert!(matches!(
            repository.register_export_root(&link, "連結"),
            Err(StorageError::InvalidExportRoot(_))
        ));
    }

    #[test]
    fn export_job_persists_one_hundred_one_items_across_reopen() {
        let tree = TestTree::new("job-reopen");
        let database = tree.0.join("catalog.db");
        let destination = tree.directory("exports");
        let job_id;
        {
            let mut repository = CatalogRepository::open(&database).expect("catalog");
            insert_collections(&repository, 101);
            let root = repository
                .register_export_root(&destination, "匯出")
                .expect("root");
            let job = repository
                .create_export_job(root.id, "C106.zip", &items(101, 4096))
                .expect("create job");
            job_id = job.id;
            assert_eq!(101, job.total_items);
            assert_eq!(101 * 4096, job.total_bytes);
        }
        let repository = CatalogRepository::open(&database).expect("reopen catalog");
        let job = repository.export_job(job_id).expect("reopened job");
        assert_eq!(ExportJobStatus::Pending, job.status);
        assert_eq!(
            101,
            repository.export_job_items(job_id).expect("items").len()
        );
        assert_eq!(Some(job), repository.latest_export_job().expect("latest"));
    }

    #[test]
    fn export_job_progress_completion_and_retry_are_persistent() {
        let tree = TestTree::new("job-lifecycle");
        let destination = tree.directory("exports");
        let mut repository = CatalogRepository::open_in_memory().expect("catalog");
        insert_collections(&repository, 2);
        let root = repository
            .register_export_root(&destination, "匯出")
            .expect("root");
        let job = repository
            .create_export_job(root.id, "package.zip", &items(2, 10))
            .expect("job");
        let job = repository.start_export_job(job.id).expect("start job");
        assert_eq!(1, job.attempts);
        repository
            .start_export_job_item(job.id, 1)
            .expect("start item");
        let job = repository
            .update_export_item_progress(job.id, 1, 4)
            .expect("progress");
        assert_eq!(4, job.processed_bytes);
        let job = repository
            .complete_export_job_item(job.id, 1)
            .expect("complete item");
        assert_eq!(1, job.processed_items);
        assert_eq!(10, job.processed_bytes);
        repository
            .start_export_job_item(job.id, 2)
            .expect("start second");
        let failed = repository
            .fail_export_job(job.id, Some(2), "來源已變更")
            .expect("fail job");
        assert_eq!(ExportJobStatus::Failed, failed.status);
        assert_eq!(1, failed.failed_items);

        let pending = repository.retry_export_job(job.id).expect("retry");
        assert_eq!(ExportJobStatus::Pending, pending.status);
        assert_eq!(0, pending.processed_items);
        assert_eq!(0, pending.processed_bytes);
        assert!(
            repository
                .export_job_items(job.id)
                .expect("reset items")
                .iter()
                .all(|item| item.status == ExportJobItemStatus::Pending)
        );
    }

    #[test]
    fn interrupted_export_job_is_failed_once_and_can_retry() {
        let tree = TestTree::new("job-recovery");
        let destination = tree.directory("exports");
        let mut repository = CatalogRepository::open_in_memory().expect("catalog");
        insert_collections(&repository, 1);
        let root = repository
            .register_export_root(&destination, "匯出")
            .expect("root");
        let job = repository
            .create_export_job(root.id, "package.zip", &items(1, 10))
            .expect("job");
        repository.start_export_job(job.id).expect("start");
        repository
            .start_export_job_item(job.id, 1)
            .expect("start item");
        assert_eq!(
            1,
            repository
                .recover_interrupted_export_jobs()
                .expect("recover")
        );
        assert_eq!(
            0,
            repository
                .recover_interrupted_export_jobs()
                .expect("idempotent")
        );
        let failed = repository.export_job(job.id).expect("failed job");
        assert_eq!(ExportJobStatus::Failed, failed.status);
        assert!(failed.error_message.is_some());
        assert_eq!(
            ExportJobStatus::Pending,
            repository.retry_export_job(job.id).expect("retry").status
        );
    }
}
