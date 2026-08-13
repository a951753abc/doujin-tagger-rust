//! Persistent manual cover selections.

use rusqlite::{OptionalExtension, params};

use super::{CatalogRepository, StorageError, StorageResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverSelectionStatus {
    Valid,
    SourceChanged,
    Missing,
}

impl CoverSelectionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::SourceChanged => "source_changed",
            Self::Missing => "missing",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "valid" => Ok(Self::Valid),
            "source_changed" => Ok(Self::SourceChanged),
            "missing" => Ok(Self::Missing),
            _ => Err(format!("未知 cover selection status：{value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverSelectionSnapshot {
    pub collection_id: i64,
    pub entry_path: String,
    pub source_fingerprint: String,
    pub validation_status: CoverSelectionStatus,
    pub validation_error: Option<String>,
    pub selected_at: String,
    pub updated_at: String,
}

impl CatalogRepository {
    pub fn cover_selection(
        &self,
        collection_id: i64,
    ) -> StorageResult<Option<CoverSelectionSnapshot>> {
        self.connection
            .query_row(
                "SELECT collection_id, entry_path, source_fingerprint, validation_status,
                        validation_error, selected_at, updated_at
                 FROM cover_selections WHERE collection_id = ?1",
                [collection_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?
            .map(|raw| {
                Ok(CoverSelectionSnapshot {
                    collection_id: raw.0,
                    entry_path: raw.1,
                    source_fingerprint: raw.2,
                    validation_status: CoverSelectionStatus::parse(&raw.3)
                        .map_err(StorageError::InvalidSchema)?,
                    validation_error: raw.4,
                    selected_at: raw.5,
                    updated_at: raw.6,
                })
            })
            .transpose()
    }

    pub fn save_cover_selection(
        &mut self,
        collection_id: i64,
        entry_path: &str,
        source_fingerprint: &str,
    ) -> StorageResult<CoverSelectionSnapshot> {
        let entry_path = entry_path.trim();
        let source_fingerprint = source_fingerprint.trim();
        if entry_path.is_empty() || source_fingerprint.is_empty() {
            return Err(StorageError::InvalidThumbnailState(
                "cover entry path 與 source fingerprint 不得為空白".to_owned(),
            ));
        }
        super::thumbnails::ensure_active_collection(&self.connection, collection_id)?;
        self.connection.execute(
            "INSERT INTO cover_selections(
                 collection_id, entry_path, source_fingerprint, validation_status,
                 validation_error, selected_at, updated_at
             ) VALUES (?1, ?2, ?3, 'valid', NULL,
                       strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                       strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             ON CONFLICT(collection_id) DO UPDATE SET
                 entry_path = excluded.entry_path,
                 source_fingerprint = excluded.source_fingerprint,
                 validation_status = 'valid', validation_error = NULL,
                 selected_at = excluded.selected_at, updated_at = excluded.updated_at",
            params![collection_id, entry_path, source_fingerprint],
        )?;
        self.cover_selection(collection_id)?
            .ok_or_else(|| StorageError::InvalidSchema("cover selection 寫入後不存在".to_owned()))
    }

    pub fn update_cover_selection_validation(
        &mut self,
        collection_id: i64,
        status: CoverSelectionStatus,
        error: Option<&str>,
    ) -> StorageResult<Option<CoverSelectionSnapshot>> {
        self.connection.execute(
            "UPDATE cover_selections
             SET validation_status = ?1, validation_error = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE collection_id = ?3",
            params![status.as_str(), error, collection_id],
        )?;
        self.cover_selection(collection_id)
    }

    pub fn clear_cover_selection(&mut self, collection_id: i64) -> StorageResult<bool> {
        super::thumbnails::ensure_active_collection(&self.connection, collection_id)?;
        Ok(self.connection.execute(
            "DELETE FROM cover_selections WHERE collection_id = ?1",
            [collection_id],
        )? > 0)
    }
}
