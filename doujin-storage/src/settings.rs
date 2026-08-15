//! Typed persistent application settings.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};

use crate::{CatalogRepository, StorageError, StorageResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredApplicationSettings {
    pub reader_path: Option<PathBuf>,
    pub thumbnail_width: u32,
    pub thumbnail_height: u32,
    pub thumbnail_quality: u8,
    pub default_archive_root_id: Option<i64>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveApplicationSettingsOutcome {
    pub settings: StoredApplicationSettings,
    pub thumbnails_requeued: usize,
}

impl CatalogRepository {
    pub fn stored_application_settings(&self) -> StorageResult<Option<StoredApplicationSettings>> {
        let raw = self
            .connection
            .query_row(
                "SELECT reader_path, thumbnail_width, thumbnail_height,
                        thumbnail_quality, default_archive_root_id, updated_at
                 FROM application_settings WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        raw.map(decode_settings).transpose()
    }

    pub fn save_application_settings(
        &mut self,
        reader_path: Option<&Path>,
        thumbnail_width: u32,
        thumbnail_height: u32,
        thumbnail_quality: u8,
        effective_thumbnail_fingerprint: &str,
        default_archive_root_id: Option<i64>,
    ) -> StorageResult<SaveApplicationSettingsOutcome> {
        validate_settings(
            reader_path,
            thumbnail_width,
            thumbnail_height,
            thumbnail_quality,
            effective_thumbnail_fingerprint,
        )?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO application_settings(
                 singleton, reader_path, thumbnail_width, thumbnail_height, thumbnail_quality,
                 default_archive_root_id
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(singleton) DO UPDATE SET
                 reader_path = excluded.reader_path,
                 thumbnail_width = excluded.thumbnail_width,
                 thumbnail_height = excluded.thumbnail_height,
                 thumbnail_quality = excluded.thumbnail_quality,
                 default_archive_root_id = excluded.default_archive_root_id,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                reader_path.map(super::path_text).transpose()?,
                i64::from(thumbnail_width),
                i64::from(thumbnail_height),
                i64::from(thumbnail_quality),
                default_archive_root_id,
            ],
        )?;
        let thumbnails_requeued = transaction.execute(
            "UPDATE thumbnail_states
             SET settings_fingerprint = ?1, status = 'pending',
                 error_kind = NULL, error_message = NULL, attempts = 0,
                 next_retry_at = NULL, failed_at = NULL,
                 generated_width = NULL, generated_height = NULL,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE settings_fingerprint <> ?1",
            [effective_thumbnail_fingerprint],
        )?;
        transaction.commit()?;
        Ok(SaveApplicationSettingsOutcome {
            settings: self
                .stored_application_settings()?
                .ok_or_else(|| StorageError::InvalidSchema("設定儲存後無法讀回".to_owned()))?,
            thumbnails_requeued,
        })
    }
}

fn validate_settings(
    reader_path: Option<&Path>,
    thumbnail_width: u32,
    thumbnail_height: u32,
    thumbnail_quality: u8,
    effective_thumbnail_fingerprint: &str,
) -> StorageResult<()> {
    if reader_path.is_some_and(|path| !path.is_absolute()) {
        return Err(StorageError::InvalidApplicationSettings(
            "reader path 必須是絕對路徑".to_owned(),
        ));
    }
    if thumbnail_width == 0
        || thumbnail_height == 0
        || thumbnail_width > 4096
        || thumbnail_height > 4096
    {
        return Err(StorageError::InvalidApplicationSettings(
            "thumbnail 尺寸必須介於 1 到 4096 像素".to_owned(),
        ));
    }
    if !(1..=100).contains(&thumbnail_quality) {
        return Err(StorageError::InvalidApplicationSettings(
            "thumbnail 品質必須介於 1 到 100".to_owned(),
        ));
    }
    if effective_thumbnail_fingerprint.trim().is_empty() {
        return Err(StorageError::InvalidApplicationSettings(
            "effective thumbnail fingerprint 不得為空白".to_owned(),
        ));
    }
    Ok(())
}

fn decode_settings(
    raw: (Option<String>, i64, i64, i64, Option<i64>, String),
) -> StorageResult<StoredApplicationSettings> {
    Ok(StoredApplicationSettings {
        reader_path: raw.0.map(PathBuf::from),
        thumbnail_width: u32::try_from(raw.1)
            .map_err(|_| StorageError::InvalidSchema("thumbnail width 超出範圍".to_owned()))?,
        thumbnail_height: u32::try_from(raw.2)
            .map_err(|_| StorageError::InvalidSchema("thumbnail height 超出範圍".to_owned()))?,
        thumbnail_quality: u8::try_from(raw.3)
            .map_err(|_| StorageError::InvalidSchema("thumbnail quality 超出範圍".to_owned()))?,
        default_archive_root_id: raw.4,
        updated_at: raw.5,
    })
}
