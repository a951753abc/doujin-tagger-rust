//! Persistent thumbnail cache and retry state.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Row, params};

use crate::{CatalogRepository, StorageError, StorageResult};

const THUMBNAIL_COLUMNS: &str =
    "collection_id, source_fingerprint, settings_fingerprint, cache_path, status,
     error_kind, error_message, attempts, next_retry_at, failed_at,
     generated_width, generated_height, created_at, updated_at, priority, requested_at";

pub const BACKGROUND_THUMBNAIL_PRIORITY: i64 = 0;
pub const DEFAULT_THUMBNAIL_PRIORITY: i64 = 1;
pub const MAX_THUMBNAIL_PRIORITY: i64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailStatus {
    Pending,
    Running,
    Ready,
    Failed,
}

impl ThumbnailStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            _ => Err(format!("未知 thumbnail status：{value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailErrorKind {
    SourceIo,
    CacheIo,
    WorkerInterrupted,
    InvalidArchive,
    NoSupportedImage,
    ImageDecode,
    ResourceLimit,
    Unsupported,
}

impl ThumbnailErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceIo => "source_io",
            Self::CacheIo => "cache_io",
            Self::WorkerInterrupted => "worker_interrupted",
            Self::InvalidArchive => "invalid_archive",
            Self::NoSupportedImage => "no_supported_image",
            Self::ImageDecode => "image_decode",
            Self::ResourceLimit => "resource_limit",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn retry_delay_seconds(self, attempts: i64) -> Option<i64> {
        let base_seconds = match self {
            Self::SourceIo | Self::WorkerInterrupted => 30_i64,
            Self::CacheIo => 60_i64,
            Self::InvalidArchive
            | Self::NoSupportedImage
            | Self::ImageDecode
            | Self::ResourceLimit
            | Self::Unsupported => return None,
        };
        let exponent = u32::try_from((attempts - 1).clamp(0, 6)).unwrap_or(6);
        Some(base_seconds.saturating_mul(1_i64 << exponent).min(60 * 60))
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "source_io" => Ok(Self::SourceIo),
            "cache_io" => Ok(Self::CacheIo),
            "worker_interrupted" => Ok(Self::WorkerInterrupted),
            "invalid_archive" => Ok(Self::InvalidArchive),
            "no_supported_image" => Ok(Self::NoSupportedImage),
            "image_decode" => Ok(Self::ImageDecode),
            "resource_limit" => Ok(Self::ResourceLimit),
            "unsupported" => Ok(Self::Unsupported),
            _ => Err(format!("未知 thumbnail error kind：{value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailStateSnapshot {
    pub collection_id: i64,
    pub source_fingerprint: String,
    pub settings_fingerprint: String,
    pub cache_path: PathBuf,
    pub status: ThumbnailStatus,
    pub error_kind: Option<ThumbnailErrorKind>,
    pub error_message: Option<String>,
    pub attempts: i64,
    pub next_retry_at: Option<String>,
    pub failed_at: Option<String>,
    pub generated_width: Option<u32>,
    pub generated_height: Option<u32>,
    pub created_at: String,
    pub updated_at: String,
    pub priority: i64,
    pub requested_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailRequestOutcome {
    pub state: ThumbnailStateSnapshot,
    pub enqueued: bool,
}

impl CatalogRepository {
    pub fn request_thumbnail(
        &mut self,
        collection_id: i64,
        source_fingerprint: &str,
        settings_fingerprint: &str,
        cache_path: &Path,
        cache_exists: bool,
    ) -> StorageResult<ThumbnailRequestOutcome> {
        self.request_thumbnail_with_priority(
            collection_id,
            source_fingerprint,
            settings_fingerprint,
            cache_path,
            cache_exists,
            DEFAULT_THUMBNAIL_PRIORITY,
        )
    }

    pub fn request_thumbnail_with_priority(
        &mut self,
        collection_id: i64,
        source_fingerprint: &str,
        settings_fingerprint: &str,
        cache_path: &Path,
        cache_exists: bool,
        priority: i64,
    ) -> StorageResult<ThumbnailRequestOutcome> {
        validate_request(source_fingerprint, settings_fingerprint, cache_path)?;
        validate_priority(priority)?;
        let transaction = self.connection.transaction()?;
        ensure_active_collection(&transaction, collection_id)?;
        let existing = raw_thumbnail_state_optional(&transaction, collection_id)?;
        let mut enqueued = false;
        match existing {
            None => {
                insert_pending(
                    &transaction,
                    collection_id,
                    source_fingerprint,
                    settings_fingerprint,
                    cache_path,
                    priority,
                )?;
                enqueued = true;
            }
            Some(raw) => {
                let state = decode_thumbnail_state(raw)?;
                let changed_input = state.source_fingerprint != source_fingerprint
                    || state.settings_fingerprint != settings_fingerprint
                    || state.cache_path != cache_path;
                let missing_ready_cache = state.status == ThumbnailStatus::Ready && !cache_exists;
                if changed_input || missing_ready_cache {
                    reset_pending(
                        &transaction,
                        collection_id,
                        source_fingerprint,
                        settings_fingerprint,
                        cache_path,
                        priority,
                    )?;
                    enqueued = true;
                } else if state.status == ThumbnailStatus::Pending {
                    prioritize_pending(&transaction, collection_id, priority)?;
                }
            }
        }
        transaction.commit()?;
        Ok(ThumbnailRequestOutcome {
            state: self.thumbnail_state(collection_id)?,
            enqueued,
        })
    }

    pub fn reset_thumbnail(
        &mut self,
        collection_id: i64,
        source_fingerprint: &str,
        settings_fingerprint: &str,
        cache_path: &Path,
    ) -> StorageResult<ThumbnailStateSnapshot> {
        validate_request(source_fingerprint, settings_fingerprint, cache_path)?;
        let transaction = self.connection.transaction()?;
        ensure_active_collection(&transaction, collection_id)?;
        if raw_thumbnail_state_optional(&transaction, collection_id)?.is_some() {
            reset_pending(
                &transaction,
                collection_id,
                source_fingerprint,
                settings_fingerprint,
                cache_path,
                DEFAULT_THUMBNAIL_PRIORITY,
            )?;
        } else {
            insert_pending(
                &transaction,
                collection_id,
                source_fingerprint,
                settings_fingerprint,
                cache_path,
                DEFAULT_THUMBNAIL_PRIORITY,
            )?;
        }
        transaction.commit()?;
        self.thumbnail_state(collection_id)
    }

    pub fn thumbnail_state(&self, collection_id: i64) -> StorageResult<ThumbnailStateSnapshot> {
        raw_thumbnail_state_optional(&self.connection, collection_id)?
            .ok_or(StorageError::ThumbnailStateNotFound(collection_id))
            .and_then(decode_thumbnail_state)
    }

    pub fn due_thumbnails(&self, limit: u32) -> StorageResult<Vec<ThumbnailStateSnapshot>> {
        self.due_thumbnails_with_min_priority(limit, BACKGROUND_THUMBNAIL_PRIORITY)
    }

    pub fn due_thumbnails_with_min_priority(
        &self,
        limit: u32,
        min_priority: i64,
    ) -> StorageResult<Vec<ThumbnailStateSnapshot>> {
        let limit = limit.clamp(1, 200);
        validate_priority(min_priority)?;
        let sql = format!(
            "SELECT {THUMBNAIL_COLUMNS} FROM thumbnail_states
             WHERE status = 'pending'
               AND priority >= ?1
               AND (next_retry_at IS NULL OR next_retry_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             ORDER BY priority DESC, COALESCE(requested_at, updated_at) ASC, collection_id DESC
             LIMIT ?2"
        );
        let mut statement = self.connection.prepare(&sql)?;
        statement
            .query_map(params![min_priority, limit], raw_thumbnail_state)?
            .map(|row| {
                row.map_err(StorageError::from)
                    .and_then(decode_thumbnail_state)
            })
            .collect()
    }

    pub fn start_thumbnail(&mut self, collection_id: i64) -> StorageResult<ThumbnailStateSnapshot> {
        let changed = self.connection.execute(
            "UPDATE thumbnail_states
             SET status = 'running', attempts = attempts + 1, next_retry_at = NULL,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE collection_id = ?1 AND status = 'pending'
               AND (next_retry_at IS NULL OR next_retry_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            [collection_id],
        )?;
        if changed == 0 {
            return Err(StorageError::ThumbnailStateUnavailable(collection_id));
        }
        self.thumbnail_state(collection_id)
    }

    pub fn complete_thumbnail(
        &mut self,
        collection_id: i64,
        width: u32,
        height: u32,
    ) -> StorageResult<ThumbnailStateSnapshot> {
        if width == 0 || height == 0 {
            return Err(StorageError::InvalidThumbnailState(
                "完成的 thumbnail 尺寸必須大於零".to_owned(),
            ));
        }
        let changed = self.connection.execute(
            "UPDATE thumbnail_states
             SET status = 'ready', error_kind = NULL, error_message = NULL,
                 next_retry_at = NULL, failed_at = NULL,
                 generated_width = ?1, generated_height = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE collection_id = ?3 AND status = 'running'",
            params![i64::from(width), i64::from(height), collection_id],
        )?;
        if changed == 0 {
            return Err(StorageError::ThumbnailStateUnavailable(collection_id));
        }
        self.thumbnail_state(collection_id)
    }

    pub fn fail_thumbnail(
        &mut self,
        collection_id: i64,
        error_kind: ThumbnailErrorKind,
        error_message: &str,
    ) -> StorageResult<ThumbnailStateSnapshot> {
        let error_message = error_message.trim();
        if error_message.is_empty() {
            return Err(StorageError::InvalidThumbnailState(
                "失敗的 thumbnail 必須包含錯誤訊息".to_owned(),
            ));
        }
        let transaction = self.connection.transaction()?;
        let attempts = transaction
            .query_row(
                "SELECT attempts FROM thumbnail_states
                 WHERE collection_id = ?1 AND status = 'running'",
                [collection_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(StorageError::ThumbnailStateUnavailable(collection_id))?;
        if let Some(delay_seconds) = error_kind.retry_delay_seconds(attempts) {
            let modifier = format!("+{delay_seconds} seconds");
            transaction.execute(
                "UPDATE thumbnail_states
                 SET status = 'pending', error_kind = ?1, error_message = ?2,
                     next_retry_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?3),
                     failed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                     generated_width = NULL, generated_height = NULL,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE collection_id = ?4",
                params![error_kind.as_str(), error_message, modifier, collection_id],
            )?;
        } else {
            transaction.execute(
                "UPDATE thumbnail_states
                 SET status = 'failed', error_kind = ?1, error_message = ?2,
                     next_retry_at = NULL,
                     failed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                     generated_width = NULL, generated_height = NULL,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE collection_id = ?3",
                params![error_kind.as_str(), error_message, collection_id],
            )?;
        }
        transaction.commit()?;
        self.thumbnail_state(collection_id)
    }

    pub fn recover_interrupted_thumbnails(&mut self) -> StorageResult<usize> {
        Ok(self.connection.execute(
            "UPDATE thumbnail_states
             SET status = 'pending', error_kind = 'worker_interrupted',
                 error_message = '先前程序在 thumbnail 完成前停止', next_retry_at = NULL,
                 failed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                 generated_width = NULL, generated_height = NULL,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE status = 'running'",
            [],
        )?)
    }

    pub fn active_collection_ids(&self) -> StorageResult<Vec<i64>> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM collections WHERE status = 'active' ORDER BY id")?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn thumbnail_collection_ids(&self) -> StorageResult<Vec<i64>> {
        let mut statement = self
            .connection
            .prepare("SELECT collection_id FROM thumbnail_states ORDER BY collection_id")?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn next_untracked_thumbnail_collection_id(&self) -> StorageResult<Option<i64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT collections.id
                 FROM collections
                 LEFT JOIN thumbnail_states ON thumbnail_states.collection_id = collections.id
                 WHERE collections.status = 'active' AND thumbnail_states.collection_id IS NULL
                 ORDER BY collections.id DESC
                 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }
}

fn validate_request(
    source_fingerprint: &str,
    settings_fingerprint: &str,
    cache_path: &Path,
) -> StorageResult<()> {
    if source_fingerprint.trim().is_empty() || settings_fingerprint.trim().is_empty() {
        return Err(StorageError::InvalidThumbnailState(
            "source 與 settings fingerprint 不得為空白".to_owned(),
        ));
    }
    if !cache_path.is_absolute() {
        return Err(StorageError::InvalidThumbnailState(
            "thumbnail cache path 必須是絕對路徑".to_owned(),
        ));
    }
    Ok(())
}

fn validate_priority(priority: i64) -> StorageResult<()> {
    if !(BACKGROUND_THUMBNAIL_PRIORITY..=MAX_THUMBNAIL_PRIORITY).contains(&priority) {
        return Err(StorageError::InvalidThumbnailState(
            "thumbnail priority 超出允許範圍".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_active_collection(
    connection: &rusqlite::Connection,
    collection_id: i64,
) -> StorageResult<()> {
    let active: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM collections WHERE id = ?1 AND status = 'active')",
        [collection_id],
        |row| row.get(0),
    )?;
    if active {
        Ok(())
    } else {
        Err(StorageError::CollectionNotFound(collection_id))
    }
}

fn insert_pending(
    connection: &rusqlite::Connection,
    collection_id: i64,
    source_fingerprint: &str,
    settings_fingerprint: &str,
    cache_path: &Path,
    priority: i64,
) -> StorageResult<()> {
    connection.execute(
        "INSERT INTO thumbnail_states(
             collection_id, source_fingerprint, settings_fingerprint, cache_path, status,
             priority, requested_at
         ) VALUES (?1, ?2, ?3, ?4, 'pending', ?5,
                   strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![
            collection_id,
            source_fingerprint,
            settings_fingerprint,
            super::path_text(cache_path)?,
            priority,
        ],
    )?;
    Ok(())
}

fn reset_pending(
    connection: &rusqlite::Connection,
    collection_id: i64,
    source_fingerprint: &str,
    settings_fingerprint: &str,
    cache_path: &Path,
    priority: i64,
) -> StorageResult<()> {
    connection.execute(
        "UPDATE thumbnail_states
         SET source_fingerprint = ?1, settings_fingerprint = ?2, cache_path = ?3,
             status = 'pending', error_kind = NULL, error_message = NULL, attempts = 0,
             next_retry_at = NULL, failed_at = NULL,
             generated_width = NULL, generated_height = NULL,
             priority = ?4, requested_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE collection_id = ?5",
        params![
            source_fingerprint,
            settings_fingerprint,
            super::path_text(cache_path)?,
            priority,
            collection_id
        ],
    )?;
    Ok(())
}

fn prioritize_pending(
    connection: &rusqlite::Connection,
    collection_id: i64,
    priority: i64,
) -> StorageResult<()> {
    connection.execute(
        "UPDATE thumbnail_states
         SET priority = MAX(priority, ?1),
             requested_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE collection_id = ?2 AND status = 'pending'",
        params![priority, collection_id],
    )?;
    Ok(())
}

type RawThumbnailState = (
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    String,
    String,
    i64,
    Option<String>,
);

fn raw_thumbnail_state(row: &Row<'_>) -> rusqlite::Result<RawThumbnailState> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
    ))
}

fn raw_thumbnail_state_optional(
    connection: &rusqlite::Connection,
    collection_id: i64,
) -> StorageResult<Option<RawThumbnailState>> {
    let sql = format!("SELECT {THUMBNAIL_COLUMNS} FROM thumbnail_states WHERE collection_id = ?1");
    Ok(connection
        .query_row(&sql, [collection_id], raw_thumbnail_state)
        .optional()?)
}

fn decode_thumbnail_state(raw: RawThumbnailState) -> StorageResult<ThumbnailStateSnapshot> {
    Ok(ThumbnailStateSnapshot {
        collection_id: raw.0,
        source_fingerprint: raw.1,
        settings_fingerprint: raw.2,
        cache_path: PathBuf::from(raw.3),
        status: ThumbnailStatus::parse(&raw.4).map_err(StorageError::InvalidSchema)?,
        error_kind: raw
            .5
            .as_deref()
            .map(ThumbnailErrorKind::parse)
            .transpose()
            .map_err(StorageError::InvalidSchema)?,
        error_message: raw.6,
        attempts: raw.7,
        next_retry_at: raw.8,
        failed_at: raw.9,
        generated_width: raw
            .10
            .map(u32::try_from)
            .transpose()
            .map_err(|_| StorageError::InvalidSchema("thumbnail width 超出範圍".to_owned()))?,
        generated_height: raw
            .11
            .map(u32::try_from)
            .transpose()
            .map_err(|_| StorageError::InvalidSchema("thumbnail height 超出範圍".to_owned()))?,
        created_at: raw.12,
        updated_at: raw.13,
        priority: raw.14,
        requested_at: raw.15,
    })
}
