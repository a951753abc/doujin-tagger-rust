//! SQLite v2 catalog and its single-writer repository boundary.

pub mod canonical;
pub mod collections;
pub mod consolidation;
pub mod covers;
pub mod duplicates;
mod exports;
pub mod external_search_batches;
pub use exports::*;
pub mod jobs;
pub mod legacy;
pub mod lifecycle;
pub mod metadata;
pub mod parser_runs;
pub mod roots;
pub mod saved_views;
pub mod scan;
pub mod settings;
pub mod statistics;
pub mod thumbnails;
pub mod vocabulary;
pub mod work_baskets;

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use doujin_scanner::{FilenameNormalization, PendingCollection, SourceKind};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::canonical::{CanonicalEntitySnapshot, CanonicalMappingEvidence, EntityKind};
use crate::lifecycle::{
    ActiveCollectionLocationSnapshot, CandidateDecision, CollectionStatus, DeleteMode,
    FileOperationKind, FileOperationSnapshot, FileOperationStatus, LocationSnapshot,
    LocationStatus, PendingFileOperation, TombstoneCandidateSnapshot,
};
use crate::metadata::{
    ExternalCandidate, ExternalCandidateOutcome, ExternalTag, ExternalTagOutcome,
    MetadataAssertionDecision, MetadataField, MetadataSource, MetadataValue, SelectionSnapshot,
};

const SCHEMA_VERSION: i64 = 17;
const INITIAL_MIGRATION: &str = include_str!("../migrations/0001_initial.sql");
const SCAN_RUN_GUARD_MIGRATION: &str = include_str!("../migrations/0002_scan_run_guard.sql");
const EXTERNAL_SEARCH_JOBS_MIGRATION: &str =
    include_str!("../migrations/0003_external_search_jobs.sql");
const COLLECTION_CONSOLIDATIONS_MIGRATION: &str =
    include_str!("../migrations/0004_collection_consolidations.sql");
const THUMBNAIL_STATES_MIGRATION: &str = include_str!("../migrations/0005_thumbnail_states.sql");
const APPLICATION_SETTINGS_MIGRATION: &str =
    include_str!("../migrations/0006_application_settings.sql");
const THUMBNAIL_PRIORITY_MIGRATION: &str =
    include_str!("../migrations/0007_thumbnail_priority.sql");
const DL_EVENT_FALLBACK_MIGRATION: &str = include_str!("../migrations/0008_dl_event_fallback.sql");
const REVERT_IS_DL_EVENT_FALLBACK_MIGRATION: &str =
    include_str!("../migrations/0009_revert_is_dl_event_fallback.sql");
const SAVED_VIEWS_MIGRATION: &str = include_str!("../migrations/0010_saved_views.sql");
const EXTERNAL_SEARCH_BATCHES_MIGRATION: &str =
    include_str!("../migrations/0011_external_search_batches.sql");
const VOCABULARY_GOVERNANCE_MIGRATION: &str =
    include_str!("../migrations/0012_vocabulary_governance.sql");
const WORK_BASKETS_MIGRATION: &str = include_str!("../migrations/0013_work_baskets.sql");
const COVER_SELECTIONS_MIGRATION: &str = include_str!("../migrations/0014_cover_selections.sql");
const DUPLICATE_DETECTION_MIGRATION: &str =
    include_str!("../migrations/0015_duplicate_detection.sql");
const EXPORT_PACKAGES_MIGRATION: &str = include_str!("../migrations/0016_export_packages.sql");
const DEFAULT_ARCHIVE_ROOT_MIGRATION: &str =
    include_str!("../migrations/0017_default_archive_root.sql");

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_initial",
        sql: INITIAL_MIGRATION,
    },
    Migration {
        version: 2,
        name: "0002_scan_run_guard",
        sql: SCAN_RUN_GUARD_MIGRATION,
    },
    Migration {
        version: 3,
        name: "0003_external_search_jobs",
        sql: EXTERNAL_SEARCH_JOBS_MIGRATION,
    },
    Migration {
        version: 4,
        name: "0004_collection_consolidations",
        sql: COLLECTION_CONSOLIDATIONS_MIGRATION,
    },
    Migration {
        version: 5,
        name: "0005_thumbnail_states",
        sql: THUMBNAIL_STATES_MIGRATION,
    },
    Migration {
        version: 6,
        name: "0006_application_settings",
        sql: APPLICATION_SETTINGS_MIGRATION,
    },
    Migration {
        version: 7,
        name: "0007_thumbnail_priority",
        sql: THUMBNAIL_PRIORITY_MIGRATION,
    },
    Migration {
        version: 8,
        name: "0008_dl_event_fallback",
        sql: DL_EVENT_FALLBACK_MIGRATION,
    },
    Migration {
        version: 9,
        name: "0009_revert_is_dl_event_fallback",
        sql: REVERT_IS_DL_EVENT_FALLBACK_MIGRATION,
    },
    Migration {
        version: 10,
        name: "0010_saved_views",
        sql: SAVED_VIEWS_MIGRATION,
    },
    Migration {
        version: 11,
        name: "0011_external_search_batches",
        sql: EXTERNAL_SEARCH_BATCHES_MIGRATION,
    },
    Migration {
        version: 12,
        name: "0012_vocabulary_governance",
        sql: VOCABULARY_GOVERNANCE_MIGRATION,
    },
    Migration {
        version: 13,
        name: "0013_work_baskets",
        sql: WORK_BASKETS_MIGRATION,
    },
    Migration {
        version: 14,
        name: "0014_cover_selections",
        sql: COVER_SELECTIONS_MIGRATION,
    },
    Migration {
        version: 15,
        name: "0015_duplicate_detection",
        sql: DUPLICATE_DETECTION_MIGRATION,
    },
    Migration {
        version: 16,
        name: "0016_export_packages",
        sql: EXPORT_PACKAGES_MIGRATION,
    },
    Migration {
        version: 17,
        name: "0017_default_archive_root",
        sql: DEFAULT_ARCHIVE_ROOT_MIGRATION,
    },
];

#[derive(Debug)]
pub enum StorageError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    NonUnicodePath(PathBuf),
    PathOutsideRoot {
        path: PathBuf,
        root: PathBuf,
    },
    UnversionedNonEmptyCatalog,
    InvalidSchema(String),
    UnsupportedSchemaVersion(i64),
    CollectionNotFound(i64),
    AssertionUnavailable(i64),
    ExternalSearchJobNotFound(i64),
    ExternalSearchJobUnavailable(i64),
    InvalidExternalSearchJob(String),
    ExternalSearchBatchNotFound(i64),
    InvalidExternalSearchBatch(String),
    InvalidMetadata(String),
    InvalidProjection {
        collection_id: i64,
        reason: String,
    },
    CanonicalEntityNotFound(i64),
    InvalidCanonicalMapping(String),
    CanonicalEntityInUse(i64),
    LibraryRootNotFound(i64),
    InvalidLibraryRoot(String),
    ExportRootNotFound(i64),
    InvalidExportRoot(String),
    ExportJobNotFound(i64),
    InvalidExportJob(String),
    InvalidLifecycle(String),
    LegacyImportRequiresEmptyCatalog,
    InvalidLegacyImport(String),
    ScanAlreadyRunning,
    ScanRunNotFound(i64),
    InvalidScanRun(String),
    ThumbnailStateNotFound(i64),
    ThumbnailStateUnavailable(i64),
    InvalidThumbnailState(String),
    InvalidApplicationSettings(String),
    SavedViewNotFound(i64),
    SavedViewNameConflict(String),
    InvalidSavedView(String),
    WorkBasketNotFound(i64),
    Ingest {
        path: PathBuf,
        source: Box<StorageError>,
    },
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "SQLite 錯誤：{error}"),
            Self::Json(error) => write!(formatter, "JSON 序列化錯誤：{error}"),
            Self::NonUnicodePath(path) => {
                write!(formatter, "路徑不是有效的 Unicode：{}", path.display())
            }
            Self::PathOutsideRoot { path, root } => write!(
                formatter,
                "收藏路徑不在掃描來源內：{}（來源：{}）",
                path.display(),
                root.display()
            ),
            Self::UnversionedNonEmptyCatalog => write!(
                formatter,
                "拒絕在非空白且沒有 v2 schema version 的 catalog 上執行 migration"
            ),
            Self::InvalidSchema(reason) => write!(formatter, "catalog schema 無效：{reason}"),
            Self::UnsupportedSchemaVersion(version) => {
                write!(formatter, "不支援的 catalog schema version：{version}")
            }
            Self::CollectionNotFound(collection_id) => {
                write!(formatter, "找不到收藏 ID：{collection_id}")
            }
            Self::AssertionUnavailable(assertion_id) => {
                write!(
                    formatter,
                    "metadata assertion 不存在或不可選擇：{assertion_id}"
                )
            }
            Self::ExternalSearchJobNotFound(job_id) => {
                write!(formatter, "找不到 external search job ID：{job_id}")
            }
            Self::ExternalSearchJobUnavailable(job_id) => {
                write!(formatter, "external search job 目前不可執行：{job_id}")
            }
            Self::InvalidExternalSearchJob(reason) => {
                write!(formatter, "external search job 無效：{reason}")
            }
            Self::ExternalSearchBatchNotFound(batch_id) => {
                write!(formatter, "找不到 external search batch ID：{batch_id}")
            }
            Self::InvalidExternalSearchBatch(reason) => {
                write!(formatter, "external search batch 無效：{reason}")
            }
            Self::InvalidMetadata(reason) => write!(formatter, "metadata 無效：{reason}"),
            Self::InvalidProjection {
                collection_id,
                reason,
            } => write!(
                formatter,
                "收藏 {collection_id} 的 effective metadata 無法重建：{reason}"
            ),
            Self::CanonicalEntityNotFound(entity_id) => {
                write!(formatter, "找不到 canonical entity ID：{entity_id}")
            }
            Self::InvalidCanonicalMapping(reason) => {
                write!(formatter, "canonical mapping 無效：{reason}")
            }
            Self::CanonicalEntityInUse(entity_id) => write!(
                formatter,
                "canonical entity {entity_id} 仍被 alias、metadata mapping 或 merge exclusion 引用"
            ),
            Self::LibraryRootNotFound(root_id) => {
                write!(formatter, "找不到 library root ID：{root_id}")
            }
            Self::InvalidLibraryRoot(reason) => {
                write!(formatter, "library root 設定無效：{reason}")
            }
            Self::ExportRootNotFound(root_id) => {
                write!(formatter, "找不到 export root ID：{root_id}")
            }
            Self::InvalidExportRoot(reason) => write!(formatter, "export root 設定無效：{reason}"),
            Self::ExportJobNotFound(job_id) => write!(formatter, "找不到 export job ID：{job_id}"),
            Self::InvalidExportJob(reason) => write!(formatter, "export job 無效：{reason}"),
            Self::InvalidLifecycle(reason) => write!(formatter, "收藏生命週期操作無效：{reason}"),
            Self::LegacyImportRequiresEmptyCatalog => {
                write!(formatter, "legacy import 只允許寫入全新的空白 v2 catalog")
            }
            Self::InvalidLegacyImport(reason) => write!(formatter, "legacy import 無效：{reason}"),
            Self::ScanAlreadyRunning => write!(formatter, "已有重新掃描工作正在執行"),
            Self::ScanRunNotFound(scan_run_id) => {
                write!(formatter, "找不到 scan run ID：{scan_run_id}")
            }
            Self::InvalidScanRun(reason) => write!(formatter, "scan run 無效：{reason}"),
            Self::ThumbnailStateNotFound(collection_id) => {
                write!(formatter, "找不到收藏 {collection_id} 的 thumbnail state")
            }
            Self::ThumbnailStateUnavailable(collection_id) => {
                write!(
                    formatter,
                    "收藏 {collection_id} 的 thumbnail state 目前不可執行"
                )
            }
            Self::InvalidThumbnailState(reason) => {
                write!(formatter, "thumbnail state 無效：{reason}")
            }
            Self::InvalidApplicationSettings(reason) => {
                write!(formatter, "application settings 無效：{reason}")
            }
            Self::SavedViewNotFound(saved_view_id) => {
                write!(formatter, "找不到 Saved View ID：{saved_view_id}")
            }
            Self::SavedViewNameConflict(name) => {
                write!(formatter, "Saved View 名稱已存在：{name}")
            }
            Self::InvalidSavedView(reason) => write!(formatter, "Saved View 無效：{reason}"),
            Self::WorkBasketNotFound(basket_id) => {
                write!(formatter, "找不到工作籃 ID：{basket_id}")
            }
            Self::Ingest { path, source } => {
                write!(formatter, "收藏入庫失敗：{}：{source}", path.display())
            }
        }
    }
}

impl Error for StorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Ingest { source, .. } => Some(source.as_ref()),
            Self::NonUnicodePath(_)
            | Self::PathOutsideRoot { .. }
            | Self::UnversionedNonEmptyCatalog
            | Self::InvalidSchema(_)
            | Self::UnsupportedSchemaVersion(_)
            | Self::CollectionNotFound(_)
            | Self::AssertionUnavailable(_)
            | Self::ExternalSearchJobNotFound(_)
            | Self::ExternalSearchJobUnavailable(_)
            | Self::InvalidExternalSearchJob(_)
            | Self::ExternalSearchBatchNotFound(_)
            | Self::InvalidExternalSearchBatch(_)
            | Self::InvalidMetadata(_)
            | Self::InvalidProjection { .. }
            | Self::CanonicalEntityNotFound(_)
            | Self::InvalidCanonicalMapping(_)
            | Self::CanonicalEntityInUse(_)
            | Self::LibraryRootNotFound(_)
            | Self::InvalidLibraryRoot(_)
            | Self::ExportRootNotFound(_)
            | Self::InvalidExportRoot(_)
            | Self::ExportJobNotFound(_)
            | Self::InvalidExportJob(_)
            | Self::InvalidLifecycle(_)
            | Self::LegacyImportRequiresEmptyCatalog
            | Self::InvalidLegacyImport(_)
            | Self::ScanAlreadyRunning
            | Self::ScanRunNotFound(_)
            | Self::InvalidScanRun(_)
            | Self::ThumbnailStateNotFound(_)
            | Self::ThumbnailStateUnavailable(_)
            | Self::InvalidThumbnailState(_)
            | Self::InvalidApplicationSettings(_)
            | Self::SavedViewNotFound(_)
            | Self::SavedViewNameConflict(_)
            | Self::InvalidSavedView(_) => None,
            Self::WorkBasketNotFound(_) => None,
        }
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub type StorageResult<T> = Result<T, StorageError>;

/// Owns the only writable connection used by the current process.
///
/// All mutation methods require `&mut self`, so scanners and future UI/background
/// producers can submit data without sharing a writable SQLite connection.
pub struct CatalogRepository {
    connection: Connection,
}

impl CatalogRepository {
    pub fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        let mut connection = Connection::open(path)?;
        configure_connection(&connection)?;
        apply_migrations(&mut connection)?;
        configure_file_catalog(&connection)?;
        Ok(Self { connection })
    }

    pub fn open_in_memory() -> StorageResult<Self> {
        let mut connection = Connection::open_in_memory()?;
        configure_connection(&connection)?;
        apply_migrations(&mut connection)?;
        Ok(Self { connection })
    }

    /// Atomically commits one scanner result. Batch policy remains outside this
    /// boundary so callers can implement all-or-nothing or partial success only
    /// after the corresponding operation has an accepted BDD rule.
    pub fn ingest_collection(
        &mut self,
        pending: &PendingCollection,
    ) -> StorageResult<IngestOutcome> {
        let transaction = self.connection.transaction()?;
        let outcome = ingest_one(&transaction, pending).map_err(|source| StorageError::Ingest {
            path: pending.path.clone(),
            source: Box::new(source),
        })?;
        transaction.commit()?;
        Ok(outcome)
    }

    pub fn set_manual_value(
        &mut self,
        collection_id: i64,
        field: MetadataField,
        value: MetadataValue,
    ) -> StorageResult<i64> {
        let value_json = value
            .into_json_for(field)
            .map_err(StorageError::InvalidMetadata)?;
        let transaction = self.connection.transaction()?;
        ensure_collection(&transaction, collection_id)?;
        transaction.execute(
            "UPDATE metadata_assertions
             SET status = 'obsolete', reason = 'replaced_by_manual_value'
             WHERE collection_id = ?1 AND field_name = ?2 AND source_kind = 'manual'
               AND status IN ('candidate', 'accepted')",
            params![collection_id, field.as_str()],
        )?;
        let assertion_id = insert_assertion(
            &transaction,
            AssertionInsert {
                collection_id,
                field,
                value_json,
                source_kind: "manual",
                status: "accepted",
                parser_run_id: None,
                source_reference: None,
                confidence_total: None,
                reason: None,
            },
        )?;
        select_assertion(&transaction, collection_id, field, assertion_id, "manual")?;
        rebuild_projection(&transaction, collection_id)?;
        transaction.commit()?;
        Ok(assertion_id)
    }

    pub fn set_inferred_value(
        &mut self,
        collection_id: i64,
        field: MetadataField,
        value: MetadataValue,
        reason: &str,
    ) -> StorageResult<i64> {
        if reason.trim().is_empty() {
            return Err(StorageError::InvalidMetadata(
                "推斷結果必須包含理由".to_owned(),
            ));
        }
        let value_json = value
            .into_json_for(field)
            .map_err(StorageError::InvalidMetadata)?;
        let transaction = self.connection.transaction()?;
        ensure_collection(&transaction, collection_id)?;
        transaction.execute(
            "UPDATE metadata_assertions
             SET status = 'obsolete', reason = 'replaced_by_new_inference'
             WHERE collection_id = ?1 AND field_name = ?2 AND source_kind = 'inference'
               AND status IN ('candidate', 'accepted')",
            params![collection_id, field.as_str()],
        )?;
        let assertion_id = insert_assertion(
            &transaction,
            AssertionInsert {
                collection_id,
                field,
                value_json,
                source_kind: "inference",
                status: "accepted",
                parser_run_id: None,
                source_reference: None,
                confidence_total: None,
                reason: Some(reason),
            },
        )?;
        reselect_by_priority(&transaction, collection_id, field, true)?;
        rebuild_projection(&transaction, collection_id)?;
        transaction.commit()?;
        Ok(assertion_id)
    }

    pub fn save_external_candidate(
        &mut self,
        candidate: ExternalCandidate,
    ) -> StorageResult<ExternalCandidateOutcome> {
        if candidate.source_reference.trim().is_empty() {
            return Err(StorageError::InvalidMetadata(
                "外部候選必須包含來源參照".to_owned(),
            ));
        }
        let value_json = candidate
            .value
            .into_json_for(candidate.field)
            .map_err(StorageError::InvalidMetadata)?;
        let confidence_json = candidate
            .confidence
            .validate_and_encode()
            .map_err(StorageError::InvalidMetadata)?;
        let transaction = self.connection.transaction()?;
        ensure_collection(&transaction, candidate.collection_id)?;

        if candidate.confidence.total < 0.75 {
            transaction.execute(
                "INSERT INTO external_search_results(
                     collection_id, field_name, value_json, source_reference,
                     confidence_total, confidence_json, disposition
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'search_only')",
                params![
                    candidate.collection_id,
                    candidate.field.as_str(),
                    value_json,
                    candidate.source_reference,
                    candidate.confidence.total,
                    confidence_json,
                ],
            )?;
            let search_result_id = transaction.last_insert_rowid();
            transaction.commit()?;
            return Ok(ExternalCandidateOutcome::SearchOnly { search_result_id });
        }

        let auto_apply = candidate.confidence.total >= 0.95
            && candidate.confidence.reliable_identifier_exact_match
            && !has_protected_selection(&transaction, candidate.collection_id, candidate.field)?;
        if let Some((assertion_id, status)) = matching_external_assertion(
            &transaction,
            candidate.collection_id,
            candidate.field,
            &value_json,
            &candidate.source_reference,
            candidate.confidence.total,
            &confidence_json,
        )? {
            let reusable = matches!(status.as_str(), "candidate" | "accepted");
            let disposition = if !reusable {
                "search_only"
            } else if auto_apply {
                "auto_applied"
            } else {
                "suggestion"
            };
            if reusable && auto_apply {
                transaction.execute(
                    "UPDATE metadata_assertions SET status = 'accepted' WHERE id = ?1",
                    [assertion_id],
                )?;
            }
            transaction.execute(
                "INSERT INTO external_search_results(
                     collection_id, field_name, value_json, source_reference,
                     confidence_total, confidence_json, disposition, assertion_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    candidate.collection_id,
                    candidate.field.as_str(),
                    &value_json,
                    &candidate.source_reference,
                    candidate.confidence.total,
                    &confidence_json,
                    disposition,
                    reusable.then_some(assertion_id),
                ],
            )?;
            let search_result_id = transaction.last_insert_rowid();
            let outcome = if !reusable {
                ExternalCandidateOutcome::SearchOnly { search_result_id }
            } else if auto_apply {
                reselect_by_priority(&transaction, candidate.collection_id, candidate.field, true)?;
                rebuild_projection(&transaction, candidate.collection_id)?;
                ExternalCandidateOutcome::AutoApplied {
                    search_result_id,
                    assertion_id,
                }
            } else {
                ExternalCandidateOutcome::Suggestion {
                    search_result_id,
                    assertion_id,
                }
            };
            transaction.commit()?;
            return Ok(outcome);
        }
        let assertion_id = insert_assertion(
            &transaction,
            AssertionInsert {
                collection_id: candidate.collection_id,
                field: candidate.field,
                value_json: value_json.clone(),
                source_kind: "external",
                status: if auto_apply { "accepted" } else { "candidate" },
                parser_run_id: None,
                source_reference: Some(&candidate.source_reference),
                confidence_total: Some(candidate.confidence.total),
                reason: Some(&candidate.confidence.reason),
            },
        )?;
        transaction.execute(
            "UPDATE metadata_assertions SET confidence_json = ?1 WHERE id = ?2",
            params![confidence_json, assertion_id],
        )?;
        transaction.execute(
            "INSERT INTO external_search_results(
                 collection_id, field_name, value_json, source_reference,
                 confidence_total, confidence_json, disposition, assertion_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                candidate.collection_id,
                candidate.field.as_str(),
                value_json,
                candidate.source_reference,
                candidate.confidence.total,
                confidence_json,
                if auto_apply {
                    "auto_applied"
                } else {
                    "suggestion"
                },
                assertion_id,
            ],
        )?;
        let search_result_id = transaction.last_insert_rowid();

        let outcome = if auto_apply {
            reselect_by_priority(&transaction, candidate.collection_id, candidate.field, true)?;
            rebuild_projection(&transaction, candidate.collection_id)?;
            ExternalCandidateOutcome::AutoApplied {
                search_result_id,
                assertion_id,
            }
        } else {
            ExternalCandidateOutcome::Suggestion {
                search_result_id,
                assertion_id,
            }
        };
        transaction.commit()?;
        Ok(outcome)
    }

    pub fn save_external_tag(&mut self, tag: ExternalTag) -> StorageResult<ExternalTagOutcome> {
        let name = tag.name.trim();
        if name.is_empty() {
            return Err(StorageError::InvalidMetadata(
                "外部 tag 名稱不得為空白".to_owned(),
            ));
        }
        if tag.source_reference.trim().is_empty() {
            return Err(StorageError::InvalidMetadata(
                "外部 tag 必須包含來源參照".to_owned(),
            ));
        }
        tag.confidence
            .validate_and_encode()
            .map_err(StorageError::InvalidMetadata)?;
        if tag.confidence.total < 0.75 {
            return Err(StorageError::InvalidMetadata(
                "外部 tag confidence 不足 0.75，不得自動加入收藏".to_owned(),
            ));
        }

        let transaction = self.connection.transaction()?;
        ensure_collection(&transaction, tag.collection_id)?;
        transaction.execute(
            "INSERT INTO tags(name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
            [name],
        )?;
        let tag_id =
            transaction.query_row("SELECT id FROM tags WHERE name = ?1", [name], |row| {
                row.get::<_, i64>(0)
            })?;
        let applied = transaction.execute(
            "INSERT INTO collection_tags(collection_id, tag_id)
             VALUES (?1, ?2) ON CONFLICT(collection_id, tag_id) DO NOTHING",
            params![tag.collection_id, tag_id],
        )? > 0;
        transaction.commit()?;
        Ok(if applied {
            ExternalTagOutcome::Applied { tag_id }
        } else {
            ExternalTagOutcome::Existing { tag_id }
        })
    }

    pub fn decide_metadata_assertion(
        &mut self,
        collection_id: i64,
        field: MetadataField,
        assertion_id: i64,
        decision: MetadataAssertionDecision,
    ) -> StorageResult<()> {
        let transaction = self.connection.transaction()?;
        ensure_collection(&transaction, collection_id)?;
        let status = transaction
            .query_row(
                "SELECT status FROM metadata_assertions
                 WHERE id = ?1 AND collection_id = ?2 AND field_name = ?3",
                params![assertion_id, collection_id, field.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StorageError::AssertionUnavailable(assertion_id))?;

        match decision {
            MetadataAssertionDecision::Select => {
                if !matches!(status.as_str(), "candidate" | "accepted") {
                    return Err(StorageError::AssertionUnavailable(assertion_id));
                }
                transaction.execute(
                    "UPDATE metadata_assertions SET status = 'accepted' WHERE id = ?1",
                    [assertion_id],
                )?;
                select_assertion(&transaction, collection_id, field, assertion_id, "manual")?;
                rebuild_projection(&transaction, collection_id)?;
            }
            MetadataAssertionDecision::Reject => {
                if status == "rejected" {
                    return Ok(());
                }
                if !matches!(status.as_str(), "candidate" | "accepted") {
                    return Err(StorageError::AssertionUnavailable(assertion_id));
                }
                let was_selected: bool = transaction.query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM metadata_selections
                         WHERE collection_id = ?1 AND field_name = ?2 AND assertion_id = ?3
                     )",
                    params![collection_id, field.as_str(), assertion_id],
                    |row| row.get(0),
                )?;
                if was_selected {
                    transaction.execute(
                        "DELETE FROM metadata_selections
                         WHERE collection_id = ?1 AND field_name = ?2",
                        params![collection_id, field.as_str()],
                    )?;
                }
                transaction.execute(
                    "UPDATE metadata_assertions SET status = 'rejected' WHERE id = ?1",
                    [assertion_id],
                )?;
                if was_selected {
                    reselect_by_priority(&transaction, collection_id, field, false)?;
                    rebuild_projection(&transaction, collection_id)?;
                }
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn clear_manual_value(
        &mut self,
        collection_id: i64,
        field: MetadataField,
    ) -> StorageResult<bool> {
        let transaction = self.connection.transaction()?;
        ensure_collection(&transaction, collection_id)?;
        let selected_manual_assertion = transaction
            .query_row(
                "SELECT selection.assertion_id
                 FROM metadata_selections AS selection
                 JOIN metadata_assertions AS assertion ON assertion.id = selection.assertion_id
                 WHERE selection.collection_id = ?1 AND selection.field_name = ?2
                   AND assertion.source_kind = 'manual'",
                params![collection_id, field.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let changed = transaction.execute(
            "UPDATE metadata_assertions
             SET status = 'obsolete', reason = 'manual_value_cleared'
             WHERE collection_id = ?1 AND field_name = ?2 AND source_kind = 'manual'
               AND status IN ('candidate', 'accepted')",
            params![collection_id, field.as_str()],
        )? > 0;
        if selected_manual_assertion.is_some() {
            transaction.execute(
                "DELETE FROM metadata_selections
                 WHERE collection_id = ?1 AND field_name = ?2",
                params![collection_id, field.as_str()],
            )?;
            reselect_by_priority(&transaction, collection_id, field, false)?;
            rebuild_projection(&transaction, collection_id)?;
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub fn rebuild_all_projections(&mut self) -> StorageResult<usize> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM effective_metadata", [])?;
        let collection_ids = {
            let mut statement = transaction.prepare("SELECT id FROM collections ORDER BY id")?;
            statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for collection_id in &collection_ids {
            rebuild_projection(&transaction, *collection_id)?;
        }
        transaction.commit()?;
        Ok(collection_ids.len())
    }

    pub fn current_selection(
        &self,
        collection_id: i64,
        field: MetadataField,
    ) -> StorageResult<Option<SelectionSnapshot>> {
        let row = self
            .connection
            .query_row(
                "SELECT assertion.id, assertion.source_kind, selection.selected_by,
                        assertion.value_json
                 FROM metadata_selections AS selection
                 JOIN metadata_assertions AS assertion ON assertion.id = selection.assertion_id
                 WHERE selection.collection_id = ?1 AND selection.field_name = ?2",
                params![collection_id, field.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(assertion_id, source, selected_by, value_json)| {
            Ok(SelectionSnapshot {
                assertion_id,
                source: MetadataSource::parse(&source).map_err(StorageError::InvalidSchema)?,
                selected_manually: selected_by == "manual",
                value_json,
            })
        })
        .transpose()
    }

    pub fn external_search_result_count(&self) -> StorageResult<i64> {
        Ok(self.connection.query_row(
            "SELECT count(*) FROM external_search_results",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn create_canonical_entity(
        &mut self,
        kind: EntityKind,
        canonical_name: &str,
        is_official: bool,
    ) -> StorageResult<i64> {
        let canonical_name = validated_canonical_name(canonical_name)?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO canonical_entities(entity_kind, canonical_name, is_official)
             VALUES (?1, ?2, ?3)",
            params![kind.as_str(), canonical_name, i64::from(is_official)],
        )?;
        let entity_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(entity_id)
    }

    pub fn canonical_entity(&self, entity_id: i64) -> StorageResult<CanonicalEntitySnapshot> {
        let row = self
            .connection
            .query_row(
                "SELECT entity_kind, canonical_name, is_official
                 FROM canonical_entities WHERE id = ?1",
                [entity_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((kind, canonical_name, is_official)) = row else {
            return Err(StorageError::CanonicalEntityNotFound(entity_id));
        };
        Ok(CanonicalEntitySnapshot {
            id: entity_id,
            kind: EntityKind::parse(&kind).map_err(StorageError::InvalidSchema)?,
            canonical_name,
            is_official,
        })
    }

    pub fn update_canonical_entity(
        &mut self,
        entity_id: i64,
        canonical_name: &str,
        is_official: bool,
    ) -> StorageResult<()> {
        let canonical_name = validated_canonical_name(canonical_name)?;
        let transaction = self.connection.transaction()?;
        ensure_canonical_entity(&transaction, entity_id)?;
        let affected_collections = mapped_collection_ids(&transaction, entity_id)?;
        transaction.execute(
            "UPDATE canonical_entities
             SET canonical_name = ?1, is_official = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?3",
            params![canonical_name, i64::from(is_official), entity_id],
        )?;
        for collection_id in affected_collections {
            rebuild_projection(&transaction, collection_id)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn add_name_variant(
        &mut self,
        entity_id: i64,
        raw_name: &str,
        source: MetadataSource,
        evidence: &CanonicalMappingEvidence,
    ) -> StorageResult<i64> {
        let raw_name = validated_raw_name(raw_name)?;
        let evidence_json = evidence
            .encode()
            .map_err(StorageError::InvalidCanonicalMapping)?;
        let transaction = self.connection.transaction()?;
        ensure_canonical_entity(&transaction, entity_id)?;
        transaction.execute(
            "INSERT INTO name_variants(entity_id, raw_name, source_kind, evidence_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(entity_id, raw_name) DO NOTHING",
            params![entity_id, raw_name, source.as_str(), evidence_json],
        )?;
        let variant_id = transaction.query_row(
            "SELECT id FROM name_variants WHERE entity_id = ?1 AND raw_name = ?2",
            params![entity_id, raw_name],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(variant_id)
    }

    pub fn remove_name_variant(&mut self, entity_id: i64, raw_name: &str) -> StorageResult<bool> {
        let transaction = self.connection.transaction()?;
        ensure_canonical_entity(&transaction, entity_id)?;
        let changed = transaction.execute(
            "DELETE FROM name_variants WHERE entity_id = ?1 AND raw_name = ?2",
            params![entity_id, raw_name],
        )? > 0;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn map_assertion_to_canonical(
        &mut self,
        assertion_id: i64,
        value_index: usize,
        raw_name: &str,
        entity_id: i64,
        evidence: &CanonicalMappingEvidence,
    ) -> StorageResult<()> {
        let raw_name = validated_raw_name(raw_name)?;
        let evidence_json = evidence
            .encode()
            .map_err(StorageError::InvalidCanonicalMapping)?;
        let value_index = i64::try_from(value_index).map_err(|_| {
            StorageError::InvalidCanonicalMapping("value index 超出支援範圍".to_owned())
        })?;
        let transaction = self.connection.transaction()?;
        let assertion = transaction
            .query_row(
                "SELECT collection_id, field_name, value_json, source_kind
                 FROM metadata_assertions WHERE id = ?1",
                [assertion_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((collection_id, field_name, value_json, source_kind)) = assertion else {
            return Err(StorageError::AssertionUnavailable(assertion_id));
        };
        let field = parse_field(&field_name)?;
        let entity_kind = canonical_entity_kind(&transaction, entity_id)?;
        validate_canonical_mapping(field, entity_kind, value_index, raw_name, &value_json)?;
        let source = MetadataSource::parse(&source_kind).map_err(StorageError::InvalidSchema)?;

        transaction.execute(
            "INSERT INTO name_variants(entity_id, raw_name, source_kind, evidence_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(entity_id, raw_name) DO NOTHING",
            params![entity_id, raw_name, source.as_str(), evidence_json],
        )?;
        transaction.execute(
            "INSERT INTO assertion_entities(
                 assertion_id, entity_id, value_index, raw_name, evidence_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(assertion_id, value_index) DO UPDATE SET
                 entity_id = excluded.entity_id,
                 raw_name = excluded.raw_name,
                 evidence_json = excluded.evidence_json",
            params![
                assertion_id,
                entity_id,
                value_index,
                raw_name,
                evidence_json
            ],
        )?;
        rebuild_projection(&transaction, collection_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn remove_assertion_canonical_mapping(
        &mut self,
        assertion_id: i64,
        value_index: usize,
    ) -> StorageResult<bool> {
        let value_index = i64::try_from(value_index).map_err(|_| {
            StorageError::InvalidCanonicalMapping("value index 超出支援範圍".to_owned())
        })?;
        let transaction = self.connection.transaction()?;
        let collection_id = transaction
            .query_row(
                "SELECT collection_id FROM metadata_assertions WHERE id = ?1",
                [assertion_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(StorageError::AssertionUnavailable(assertion_id))?;
        let changed = transaction.execute(
            "DELETE FROM assertion_entities WHERE assertion_id = ?1 AND value_index = ?2",
            params![assertion_id, value_index],
        )? > 0;
        if changed {
            rebuild_projection(&transaction, collection_id)?;
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub fn exclude_entity_merge(
        &mut self,
        first_entity_id: i64,
        second_entity_id: i64,
        reason: &str,
    ) -> StorageResult<()> {
        if reason.trim().is_empty() {
            return Err(StorageError::InvalidCanonicalMapping(
                "拒絕合併必須包含理由".to_owned(),
            ));
        }
        let (left_entity_id, right_entity_id) =
            normalized_entity_pair(first_entity_id, second_entity_id)?;
        let transaction = self.connection.transaction()?;
        let left_kind = canonical_entity_kind(&transaction, left_entity_id)?;
        let right_kind = canonical_entity_kind(&transaction, right_entity_id)?;
        if left_kind != right_kind {
            return Err(StorageError::InvalidCanonicalMapping(
                "不同 entity kind 不能成為名稱合併候選".to_owned(),
            ));
        }
        transaction.execute(
            "INSERT INTO merge_exclusions(left_entity_id, right_entity_id, reason)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(left_entity_id, right_entity_id) DO UPDATE SET reason = excluded.reason",
            params![left_entity_id, right_entity_id, reason],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn is_entity_merge_excluded(
        &self,
        first_entity_id: i64,
        second_entity_id: i64,
    ) -> StorageResult<bool> {
        let first = self.canonical_entity(first_entity_id)?;
        let second = self.canonical_entity(second_entity_id)?;
        if first.kind != second.kind {
            return Err(StorageError::InvalidCanonicalMapping(
                "不同 entity kind 不能成為名稱合併候選".to_owned(),
            ));
        }
        let (left_entity_id, right_entity_id) =
            normalized_entity_pair(first_entity_id, second_entity_id)?;
        Ok(self.connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM merge_exclusions
                 WHERE left_entity_id = ?1 AND right_entity_id = ?2
             )",
            params![left_entity_id, right_entity_id],
            |row| row.get(0),
        )?)
    }

    pub fn remove_entity_merge_exclusion(
        &mut self,
        first_entity_id: i64,
        second_entity_id: i64,
    ) -> StorageResult<bool> {
        let (left_entity_id, right_entity_id) =
            normalized_entity_pair(first_entity_id, second_entity_id)?;
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "DELETE FROM merge_exclusions
             WHERE left_entity_id = ?1 AND right_entity_id = ?2",
            params![left_entity_id, right_entity_id],
        )? > 0;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn delete_canonical_entity(&mut self, entity_id: i64) -> StorageResult<()> {
        let transaction = self.connection.transaction()?;
        ensure_canonical_entity(&transaction, entity_id)?;
        let reference_count: i64 = transaction.query_row(
            "SELECT
                 (SELECT count(*) FROM name_variants WHERE entity_id = ?1) +
                 (SELECT count(*) FROM assertion_entities WHERE entity_id = ?1) +
                 (SELECT count(*) FROM merge_exclusions
                  WHERE left_entity_id = ?1 OR right_entity_id = ?1)",
            [entity_id],
            |row| row.get(0),
        )?;
        if reference_count > 0 {
            return Err(StorageError::CanonicalEntityInUse(entity_id));
        }
        transaction.execute("DELETE FROM canonical_entities WHERE id = ?1", [entity_id])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn effective_authors(&self, collection_id: i64) -> StorageResult<Vec<String>> {
        let authors_json = self
            .connection
            .query_row(
                "SELECT authors_json FROM effective_metadata WHERE collection_id = ?1",
                [collection_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StorageError::CollectionNotFound(collection_id))?;
        Ok(serde_json::from_str(&authors_json)?)
    }

    pub fn register_library_root(
        &mut self,
        path: &Path,
        source: SourceKind,
        label: &str,
    ) -> StorageResult<i64> {
        roots::validate_library_root(path, label)?;
        let transaction = self.connection.transaction()?;
        let root_id = upsert_library_root(&transaction, path, source, label)?;
        transaction.commit()?;
        Ok(root_id)
    }

    /// Records a move only after the file service has successfully moved the ZIP.
    /// This method does not mutate the filesystem.
    pub fn record_completed_system_move(
        &mut self,
        collection_id: i64,
        archive_root_id: i64,
        destination: &Path,
    ) -> StorageResult<i64> {
        if !destination.is_file() {
            return Err(StorageError::InvalidLifecycle(format!(
                "搬移後的目標 ZIP 不存在：{}",
                destination.display()
            )));
        }
        let transaction = self.connection.transaction()?;
        let current = transaction
            .query_row(
                "SELECT location.id, location.full_path, root.source_kind
                 FROM collection_locations AS location
                 JOIN library_roots AS root ON root.id = location.root_id
                 WHERE location.collection_id = ?1 AND location.location_status = 'current'",
                [collection_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::InvalidLifecycle(format!("收藏 {collection_id} 沒有目前位置"))
            })?;
        let (location_id, source_path, source_kind) = current;
        if source_kind != "downloads" {
            return Err(StorageError::InvalidLifecycle(
                "系統 move 只能從下載區開始".to_owned(),
            ));
        }
        if Path::new(&source_path).exists() {
            return Err(StorageError::InvalidLifecycle(format!(
                "來源檔案仍存在，不能記錄為已完成 move：{source_path}"
            )));
        }

        let archive_root = transaction
            .query_row(
                "SELECT path, source_kind, active FROM library_roots WHERE id = ?1",
                [archive_root_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::InvalidLifecycle(format!(
                    "找不到 archive library root：{archive_root_id}"
                ))
            })?;
        let (archive_root_path, archive_source_kind, archive_active) = archive_root;
        if archive_source_kind != "archive" || !archive_active {
            return Err(StorageError::InvalidLifecycle(
                "move 目標必須是啟用中的歸檔區".to_owned(),
            ));
        }
        let archive_root_path = Path::new(&archive_root_path);
        let canonical_destination = fs::canonicalize(destination).map_err(|error| {
            StorageError::InvalidLifecycle(format!(
                "無法解析 move 目標路徑 {}：{error}",
                destination.display()
            ))
        })?;
        let canonical_archive_root = fs::canonicalize(archive_root_path).map_err(|error| {
            StorageError::InvalidLifecycle(format!(
                "無法解析歸檔區路徑 {}：{error}",
                archive_root_path.display()
            ))
        })?;
        if !canonical_destination.starts_with(&canonical_archive_root) {
            return Err(StorageError::InvalidLifecycle(format!(
                "move 目標解析後不在指定歸檔區內：{}",
                destination.display()
            )));
        }
        let relative_path = destination.strip_prefix(archive_root_path).map_err(|_| {
            StorageError::InvalidLifecycle(format!(
                "move 目標不在指定歸檔區內：{}",
                destination.display()
            ))
        })?;
        let filename = destination
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| StorageError::NonUnicodePath(destination.to_owned()))?;
        let destination_text = path_text(destination)?;
        let destination_key = path_key(destination);
        let destination_conflict: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM collection_locations
                 WHERE path_key = ?1 AND location_status = 'current'
             )",
            [&destination_key],
            |row| row.get(0),
        )?;
        if destination_conflict {
            return Err(StorageError::InvalidLifecycle(format!(
                "move 目標已被其他收藏索引：{}",
                destination.display()
            )));
        }

        transaction.execute(
            "UPDATE collection_locations
             SET location_status = 'moved', ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [location_id],
        )?;
        transaction.execute(
            "INSERT INTO collection_locations(
                 collection_id, root_id, full_path, path_key, relative_path,
                 filename, location_status
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'current')",
            params![
                collection_id,
                archive_root_id,
                destination_text,
                destination_key,
                path_text(relative_path)?,
                filename,
            ],
        )?;
        transaction.execute(
            "UPDATE collections
             SET status = 'active', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [collection_id],
        )?;
        transaction.execute(
            "INSERT INTO file_operations(
                 collection_id, operation_kind, from_path, to_path, status, completed_at
             ) VALUES (?1, 'move', ?2, ?3, 'succeeded',
                       strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            params![collection_id, source_path, destination_text],
        )?;
        let operation_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(operation_id)
    }

    pub fn mark_collection_missing(&mut self, collection_id: i64) -> StorageResult<()> {
        let transaction = self.connection.transaction()?;
        let location = transaction
            .query_row(
                "SELECT id, full_path FROM collection_locations
                 WHERE collection_id = ?1 AND location_status = 'current'",
                [collection_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::InvalidLifecycle(format!(
                    "收藏 {collection_id} 沒有可標記為 missing 的目前位置"
                ))
            })?;
        if Path::new(&location.1).exists() {
            return Err(StorageError::InvalidLifecycle(format!(
                "收藏檔案仍存在，不能標記為 missing：{}",
                location.1
            )));
        }
        transaction.execute(
            "UPDATE collection_locations
             SET location_status = 'missing', ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [location.0],
        )?;
        transaction.execute(
            "UPDATE collections
             SET status = 'tombstone', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [collection_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn active_collection_locations(
        &self,
    ) -> StorageResult<Vec<ActiveCollectionLocationSnapshot>> {
        let mut statement = self.connection.prepare(
            "SELECT collection.id, location.full_path, root.path
             FROM collections AS collection
             JOIN collection_locations AS location ON location.collection_id = collection.id
             JOIN library_roots AS root ON root.id = location.root_id
             WHERE collection.status = 'active'
               AND location.location_status = 'current'
             ORDER BY collection.id",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(ActiveCollectionLocationSnapshot {
                    collection_id: row.get(0)?,
                    path: PathBuf::from(row.get::<_, String>(1)?),
                    root_path: PathBuf::from(row.get::<_, String>(2)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn link_tombstones_to_active_same_filename(&mut self) -> StorageResult<usize> {
        let transaction = self.connection.transaction()?;
        let created = transaction.execute(
            "INSERT INTO tombstone_candidates(
                 tombstone_collection_id, candidate_collection_id, reason
             )
             SELECT DISTINCT tombstone.id, candidate.id, 'same_filename'
             FROM collections AS tombstone
             JOIN collection_locations AS missing_location
               ON missing_location.collection_id = tombstone.id
              AND missing_location.location_status = 'missing'
             JOIN collection_locations AS candidate_location
               ON candidate_location.filename = missing_location.filename COLLATE NOCASE
              AND candidate_location.location_status = 'current'
             JOIN collections AS candidate
               ON candidate.id = candidate_location.collection_id
              AND candidate.status = 'active'
             WHERE tombstone.status = 'tombstone'
               AND tombstone.id <> candidate.id
             ON CONFLICT(tombstone_collection_id, candidate_collection_id) DO NOTHING",
            [],
        )?;
        transaction.commit()?;
        Ok(created)
    }

    pub fn collection_status(&self, collection_id: i64) -> StorageResult<CollectionStatus> {
        let status = self
            .connection
            .query_row(
                "SELECT status FROM collections WHERE id = ?1",
                [collection_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StorageError::CollectionNotFound(collection_id))?;
        CollectionStatus::parse(&status).map_err(StorageError::InvalidSchema)
    }

    pub fn location_history(&self, collection_id: i64) -> StorageResult<Vec<LocationSnapshot>> {
        let mut statement = self.connection.prepare(
            "SELECT full_path, location_status, root_id
             FROM collection_locations WHERE collection_id = ?1 ORDER BY id",
        )?;
        let rows = statement
            .query_map([collection_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(path, status, root_id)| {
                Ok(LocationSnapshot {
                    path: PathBuf::from(path),
                    status: LocationStatus::parse(&status).map_err(StorageError::InvalidSchema)?,
                    root_id,
                })
            })
            .collect()
    }

    pub fn collection_id_for_current_path(&self, path: &Path) -> StorageResult<Option<i64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT collection_id FROM collection_locations
                 WHERE path_key = ?1 AND location_status = 'current'",
                [path_key(path)],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn tombstone_candidates(
        &self,
        tombstone_collection_id: i64,
    ) -> StorageResult<Vec<TombstoneCandidateSnapshot>> {
        self.query_tombstone_candidates(Some(tombstone_collection_id))
    }

    pub fn all_tombstone_candidates(&self) -> StorageResult<Vec<TombstoneCandidateSnapshot>> {
        self.query_tombstone_candidates(None)
    }

    pub fn tombstone_candidate_count(&self) -> StorageResult<usize> {
        let count =
            self.connection
                .query_row("SELECT count(*) FROM tombstone_candidates", [], |row| {
                    row.get::<_, i64>(0)
                })?;
        usize::try_from(count).map_err(|_| {
            StorageError::InvalidSchema("tombstone candidate count 超出範圍".to_owned())
        })
    }

    fn query_tombstone_candidates(
        &self,
        tombstone_collection_id: Option<i64>,
    ) -> StorageResult<Vec<TombstoneCandidateSnapshot>> {
        let mut statement = self.connection.prepare(
            "SELECT candidate_link.tombstone_collection_id,
                    candidate_link.candidate_collection_id,
                    tombstone_location.full_path,
                    candidate_location.full_path,
                    candidate_link.reason,
                    candidate_link.decision,
                    candidate_link.discovered_at,
                    candidate_link.decided_at
             FROM tombstone_candidates AS candidate_link
             JOIN collection_locations AS tombstone_location
               ON tombstone_location.id = (
                    SELECT max(location.id)
                    FROM collection_locations AS location
                    WHERE location.collection_id = candidate_link.tombstone_collection_id
               )
             LEFT JOIN collection_locations AS candidate_location
               ON candidate_location.collection_id = candidate_link.candidate_collection_id
              AND candidate_location.location_status = 'current'
             WHERE (?1 IS NULL OR candidate_link.tombstone_collection_id = ?1)
             ORDER BY candidate_link.tombstone_collection_id,
                      candidate_link.candidate_collection_id",
        )?;
        let rows = statement
            .query_map([tombstone_collection_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(
                    tombstone_id,
                    candidate_id,
                    tombstone_path,
                    candidate_path,
                    reason,
                    decision,
                    discovered_at,
                    decided_at,
                )| {
                    Ok(TombstoneCandidateSnapshot {
                        tombstone_collection_id: tombstone_id,
                        candidate_collection_id: candidate_id,
                        tombstone_path: PathBuf::from(tombstone_path),
                        candidate_path: candidate_path.map(PathBuf::from),
                        reason,
                        decision: CandidateDecision::parse(&decision)
                            .map_err(StorageError::InvalidSchema)?,
                        discovered_at,
                        decided_at,
                    })
                },
            )
            .collect()
    }

    pub fn decide_tombstone_candidate(
        &mut self,
        tombstone_collection_id: i64,
        candidate_collection_id: i64,
        decision: CandidateDecision,
    ) -> StorageResult<()> {
        if decision == CandidateDecision::Pending {
            return Err(StorageError::InvalidLifecycle(
                "人工裁決必須是 confirmed 或 rejected".to_owned(),
            ));
        }
        if let Some(survivor_id) = self.merged_into_collection(candidate_collection_id)? {
            if survivor_id == tombstone_collection_id && decision == CandidateDecision::Confirmed {
                return Ok(());
            }
            return Err(StorageError::InvalidLifecycle(format!(
                "candidate 已合併至收藏 {survivor_id}，既有裁決不可變更"
            )));
        }
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE tombstone_candidates
             SET decision = ?1, decided_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE tombstone_collection_id = ?2 AND candidate_collection_id = ?3",
            params![
                decision.as_str(),
                tombstone_collection_id,
                candidate_collection_id
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidLifecycle(
                "找不到指定的 tombstone candidate 關聯".to_owned(),
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn file_operation_count(&self) -> StorageResult<i64> {
        Ok(self
            .connection
            .query_row("SELECT count(*) FROM file_operations", [], |row| row.get(0))?)
    }

    pub fn active_collection_file_path(&self, collection_id: i64) -> StorageResult<PathBuf> {
        Ok(current_location_for_operation(&self.connection, collection_id)?.path)
    }

    pub fn begin_system_move(
        &mut self,
        collection_id: i64,
        archive_root_id: i64,
        destination: &Path,
    ) -> StorageResult<PendingFileOperation> {
        if destination.exists() {
            return Err(StorageError::InvalidLifecycle(format!(
                "move 目標已存在，禁止覆寫：{}",
                destination.display()
            )));
        }
        let transaction = self.connection.transaction()?;
        let current = current_location_for_operation(&transaction, collection_id)?;
        if current.source_kind != "downloads" {
            return Err(StorageError::InvalidLifecycle(
                "系統 move 只能從下載區開始".to_owned(),
            ));
        }
        if !current.path.is_file() {
            return Err(StorageError::InvalidLifecycle(format!(
                "move 來源 ZIP 不存在：{}",
                current.path.display()
            )));
        }
        validate_archive_destination(&transaction, archive_root_id, destination, false)?;
        let destination_key = path_key(destination);
        let destination_conflict: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM collection_locations
                 WHERE path_key = ?1 AND location_status = 'current'
             )",
            [&destination_key],
            |row| row.get(0),
        )?;
        if destination_conflict {
            return Err(StorageError::InvalidLifecycle(format!(
                "move 目標已被收藏索引：{}",
                destination.display()
            )));
        }
        transaction.execute(
            "INSERT INTO file_operations(
                 collection_id, from_location_id, to_root_id, operation_kind,
                 from_path, to_path, status
             ) VALUES (?1, ?2, ?3, 'move', ?4, ?5, 'pending')",
            params![
                collection_id,
                current.location_id,
                archive_root_id,
                path_text(&current.path)?,
                path_text(destination)?,
            ],
        )?;
        let operation = PendingFileOperation {
            id: transaction.last_insert_rowid(),
            collection_id,
            kind: FileOperationKind::Move,
            from_path: current.path,
            to_path: Some(destination.to_owned()),
        };
        transaction.commit()?;
        Ok(operation)
    }

    pub fn begin_rename(
        &mut self,
        collection_id: i64,
        expected_source: &Path,
        destination: &Path,
    ) -> StorageResult<PendingFileOperation> {
        let transaction = self.connection.transaction()?;
        let current = current_location_for_operation(&transaction, collection_id)?;
        if current.media_kind != "zip" {
            return Err(StorageError::InvalidLifecycle(
                "圖片資料夾尚未接入正式檔案操作 lifecycle，第一版批次改名只支援 ZIP".to_owned(),
            ));
        }
        if current.path != expected_source {
            return Err(StorageError::InvalidLifecycle(format!(
                "rename 來源已變更：預期 {}，目前 {}",
                expected_source.display(),
                current.path.display()
            )));
        }
        if !current.path.is_file() {
            return Err(StorageError::InvalidLifecycle(format!(
                "rename 來源 ZIP 不存在：{}",
                current.path.display()
            )));
        }
        let source_parent = current.path.parent().ok_or_else(|| {
            StorageError::InvalidLifecycle("rename 來源缺少 parent directory".to_owned())
        })?;
        let destination_parent = destination.parent().ok_or_else(|| {
            StorageError::InvalidLifecycle("rename 目標缺少 parent directory".to_owned())
        })?;
        if path_key(source_parent) != path_key(destination_parent) {
            return Err(StorageError::InvalidLifecycle(
                "rename 只允許在相同 parent directory 內變更名稱；跨目錄請使用 move".to_owned(),
            ));
        }
        if !destination
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            return Err(StorageError::InvalidLifecycle(
                "rename 目標必須是 ZIP 檔案".to_owned(),
            ));
        }
        if destination.exists() {
            return Err(StorageError::InvalidLifecycle(format!(
                "rename 目標已存在，禁止覆寫：{}",
                destination.display()
            )));
        }
        let destination_key = path_key(destination);
        let destination_conflict: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM collection_locations
                 WHERE path_key = ?1 AND location_status = 'current'
                   AND collection_id <> ?2
             )",
            params![destination_key, collection_id],
            |row| row.get(0),
        )?;
        if destination_conflict {
            return Err(StorageError::InvalidLifecycle(format!(
                "rename 目標已被其他收藏索引：{}",
                destination.display()
            )));
        }
        transaction.execute(
            "INSERT INTO file_operations(
                 collection_id, from_location_id, operation_kind,
                 from_path, to_path, status
             ) VALUES (?1, ?2, 'rename', ?3, ?4, 'pending')",
            params![
                collection_id,
                current.location_id,
                path_text(&current.path)?,
                path_text(destination)?,
            ],
        )?;
        let operation = PendingFileOperation {
            id: transaction.last_insert_rowid(),
            collection_id,
            kind: FileOperationKind::Rename,
            from_path: current.path,
            to_path: Some(destination.to_owned()),
        };
        transaction.commit()?;
        Ok(operation)
    }

    pub fn begin_delete(
        &mut self,
        collection_id: i64,
        mode: DeleteMode,
    ) -> StorageResult<PendingFileOperation> {
        let transaction = self.connection.transaction()?;
        let current = current_location_for_operation(&transaction, collection_id)?;
        if !current.path.is_file() {
            return Err(StorageError::InvalidLifecycle(format!(
                "刪除來源 ZIP 不存在：{}",
                current.path.display()
            )));
        }
        transaction.execute(
            "INSERT INTO file_operations(
                 collection_id, from_location_id, operation_kind, from_path, status
             ) VALUES (?1, ?2, ?3, ?4, 'pending')",
            params![
                collection_id,
                current.location_id,
                mode.operation_kind(),
                path_text(&current.path)?,
            ],
        )?;
        let operation = PendingFileOperation {
            id: transaction.last_insert_rowid(),
            collection_id,
            kind: match mode {
                DeleteMode::Soft => FileOperationKind::SoftDelete,
                DeleteMode::Permanent => FileOperationKind::HardDelete,
            },
            from_path: current.path,
            to_path: None,
        };
        transaction.commit()?;
        Ok(operation)
    }

    pub fn complete_file_operation(&mut self, operation_id: i64) -> StorageResult<()> {
        let transaction = self.connection.transaction()?;
        let operation = pending_operation_row(&transaction, operation_id)?;
        match operation.kind {
            FileOperationKind::Rename => complete_pending_rename(&transaction, &operation)?,
            FileOperationKind::Move => complete_pending_move(&transaction, &operation)?,
            FileOperationKind::SoftDelete => {
                complete_pending_delete(&transaction, &operation, false)?
            }
            FileOperationKind::HardDelete => {
                complete_pending_delete(&transaction, &operation, true)?
            }
        }
        transaction.execute(
            "UPDATE file_operations
             SET status = 'succeeded', completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1 AND status = 'pending'",
            [operation_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn fail_file_operation(
        &mut self,
        operation_id: i64,
        error_message: &str,
    ) -> StorageResult<()> {
        if error_message.trim().is_empty() {
            return Err(StorageError::InvalidLifecycle(
                "file operation failure 必須包含錯誤訊息".to_owned(),
            ));
        }
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE file_operations
             SET status = 'failed', error_message = ?1,
                 completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?2 AND status = 'pending'",
            params![error_message, operation_id],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidLifecycle(format!(
                "file operation {operation_id} 不存在或已完成"
            )));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn file_operation(&self, operation_id: i64) -> StorageResult<FileOperationSnapshot> {
        let row = self
            .connection
            .query_row(
                "SELECT collection_id, operation_kind, status, from_path, to_path, error_message
                 FROM file_operations WHERE id = ?1",
                [operation_id],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::InvalidLifecycle(format!("找不到 file operation：{operation_id}"))
            })?;
        Ok(FileOperationSnapshot {
            id: operation_id,
            collection_id: row.0,
            kind: FileOperationKind::parse(&row.1).map_err(StorageError::InvalidSchema)?,
            status: FileOperationStatus::parse(&row.2).map_err(StorageError::InvalidSchema)?,
            from_path: PathBuf::from(row.3),
            to_path: row.4.map(PathBuf::from),
            error_message: row.5,
        })
    }

    pub fn pending_file_operations(&self) -> StorageResult<Vec<PendingFileOperation>> {
        let mut statement = self.connection.prepare(
            "SELECT id, collection_id, operation_kind, from_path, to_path
             FROM file_operations WHERE status = 'pending' ORDER BY id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, collection_id, kind, from_path, to_path)| {
                Ok(PendingFileOperation {
                    id,
                    collection_id: collection_id.ok_or_else(|| {
                        StorageError::InvalidSchema(format!(
                            "pending file operation {id} 缺少 collection ID"
                        ))
                    })?,
                    kind: FileOperationKind::parse(&kind).map_err(StorageError::InvalidSchema)?,
                    from_path: PathBuf::from(from_path),
                    to_path: to_path.map(PathBuf::from),
                })
            })
            .collect()
    }

    pub fn schema_version(&self) -> StorageResult<i64> {
        Ok(self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub fn sqlite_version(&self) -> StorageResult<String> {
        Ok(self
            .connection
            .query_row("SELECT sqlite_version()", [], |row| row.get(0))?)
    }

    pub fn journal_mode(&self) -> StorageResult<String> {
        Ok(self
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))?)
    }

    pub fn collection_count(&self) -> StorageResult<i64> {
        Ok(self
            .connection
            .query_row("SELECT count(*) FROM collections", [], |row| row.get(0))?)
    }

    pub fn parser_run_count(&self) -> StorageResult<i64> {
        Ok(self
            .connection
            .query_row("SELECT count(*) FROM parser_runs", [], |row| row.get(0))?)
    }

    pub fn assertion_count(&self) -> StorageResult<i64> {
        Ok(self
            .connection
            .query_row("SELECT count(*) FROM metadata_assertions", [], |row| {
                row.get(0)
            })?)
    }

    pub fn library_root_count(&self) -> StorageResult<i64> {
        Ok(self
            .connection
            .query_row("SELECT count(*) FROM library_roots", [], |row| row.get(0))?)
    }

    pub fn effective_metadata_count(&self) -> StorageResult<i64> {
        Ok(self
            .connection
            .query_row("SELECT count(*) FROM effective_metadata", [], |row| {
                row.get(0)
            })?)
    }

    pub fn foreign_keys_enabled(&self) -> StorageResult<bool> {
        let enabled: i64 = self
            .connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
        Ok(enabled == 1)
    }

    pub fn table_is_strict(&self, table_name: &str) -> StorageResult<bool> {
        let strict: Option<i64> = self
            .connection
            .query_row(
                "SELECT strict FROM pragma_table_list WHERE name = ?1",
                [table_name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(strict == Some(1))
    }

    pub fn current_paths(&self) -> StorageResult<HashSet<PathBuf>> {
        let mut statement = self.connection.prepare(
            "SELECT full_path FROM collection_locations WHERE location_status = 'current'",
        )?;
        let paths = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(paths.into_iter().map(PathBuf::from).collect())
    }

    pub fn current_value_json(
        &self,
        collection_id: i64,
        field_name: &str,
    ) -> StorageResult<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT assertion.value_json
                 FROM metadata_selections AS selection
                 JOIN metadata_assertions AS assertion ON assertion.id = selection.assertion_id
                 WHERE selection.collection_id = ?1 AND selection.field_name = ?2",
                params![collection_id, field_name],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn first_collection_id(&self) -> StorageResult<Option<i64>> {
        Ok(self
            .connection
            .query_row("SELECT min(id) FROM collections", [], |row| row.get(0))?)
    }

    pub fn first_parser_raw_filename(&self) -> StorageResult<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT raw_filename FROM parser_runs ORDER BY id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn search_titles(&self, query: &str) -> StorageResult<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT metadata.title
             FROM collection_fts
             JOIN effective_metadata AS metadata ON metadata.collection_id = collection_fts.rowid
             JOIN collections AS collection ON collection.id = metadata.collection_id
             WHERE collection_fts MATCH ?1 AND collection.status = 'active'
             ORDER BY rank, metadata.collection_id",
        )?;
        Ok(statement
            .query_map([query], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    Inserted,
    SkippedExisting,
}

fn configure_connection(connection: &Connection) -> StorageResult<()> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

fn configure_file_catalog(connection: &Connection) -> StorageResult<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

fn apply_migrations(connection: &mut Connection) -> StorageResult<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if !(0..=SCHEMA_VERSION).contains(&version) {
        return Err(StorageError::UnsupportedSchemaVersion(version));
    }

    if version == 0 {
        let existing_tables: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if existing_tables > 0 {
            return Err(StorageError::UnversionedNonEmptyCatalog);
        }
    } else {
        validate_migration_records(connection, version)?;
    }

    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version > version)
    {
        let transaction = connection.transaction()?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, name) VALUES (?1, ?2)",
            params![migration.version, migration.name],
        )?;
        transaction.pragma_update(None, "user_version", migration.version)?;
        transaction.commit()?;
    }
    validate_schema(connection)
}

fn validate_schema(connection: &Connection) -> StorageResult<()> {
    validate_migration_records(connection, SCHEMA_VERSION)
}

fn validate_migration_records(connection: &Connection, through_version: i64) -> StorageResult<()> {
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= through_version)
    {
        let migration_name = connection
            .query_row(
                "SELECT name FROM schema_migrations WHERE version = ?1",
                [migration.version],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if migration_name.as_deref() != Some(migration.name) {
            return Err(StorageError::InvalidSchema(format!(
                "找不到 {} migration 紀錄",
                migration.name
            )));
        }
    }
    Ok(())
}

fn ensure_collection(transaction: &Transaction<'_>, collection_id: i64) -> StorageResult<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM collections WHERE id = ?1)",
        [collection_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(StorageError::CollectionNotFound(collection_id));
    }
    Ok(())
}

struct CurrentOperationLocation {
    location_id: i64,
    path: PathBuf,
    root_path: PathBuf,
    source_kind: String,
    root_active: bool,
    media_kind: String,
}

struct PendingOperationRow {
    collection_id: i64,
    from_location_id: i64,
    to_root_id: Option<i64>,
    kind: FileOperationKind,
    from_path: PathBuf,
    to_path: Option<PathBuf>,
}

struct ArchiveDestination {
    relative_path: String,
    filename: String,
}

fn current_location_for_operation(
    connection: &Connection,
    collection_id: i64,
) -> StorageResult<CurrentOperationLocation> {
    let row = connection
        .query_row(
            "SELECT location.id, location.full_path, root.path, root.source_kind, root.active,
                    collection.media_kind
             FROM collection_locations AS location
             JOIN library_roots AS root ON root.id = location.root_id
             JOIN collections AS collection ON collection.id = location.collection_id
             WHERE location.collection_id = ?1
               AND location.location_status = 'current'
               AND collection.status = 'active'",
            [collection_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::InvalidLifecycle(format!(
                "收藏 {collection_id} 沒有可操作的 active current location"
            ))
        })?;
    let current = CurrentOperationLocation {
        location_id: row.0,
        path: PathBuf::from(row.1),
        root_path: PathBuf::from(row.2),
        source_kind: row.3,
        root_active: row.4,
        media_kind: row.5,
    };
    validate_active_source_location(&current)?;
    Ok(current)
}

fn validate_active_source_location(current: &CurrentOperationLocation) -> StorageResult<()> {
    if !current.root_active {
        return Err(StorageError::InvalidLifecycle(
            "收藏來源已停用，拒絕檔案操作".to_owned(),
        ));
    }
    let root_key = path_key(&current.root_path);
    let path_key = path_key(&current.path);
    let root_prefix = format!("{root_key}\\");
    if !path_key.starts_with(&root_prefix) {
        return Err(StorageError::InvalidLifecycle(format!(
            "收藏路徑不在目前設定的來源內：{}",
            current.path.display()
        )));
    }
    let canonical_root = fs::canonicalize(&current.root_path).map_err(|error| {
        StorageError::InvalidLifecycle(format!(
            "無法解析收藏來源 {}：{error}",
            current.root_path.display()
        ))
    })?;
    let parent = current.path.parent().ok_or_else(|| {
        StorageError::InvalidLifecycle("收藏路徑缺少 parent directory".to_owned())
    })?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| {
        StorageError::InvalidLifecycle(format!(
            "無法解析收藏所在資料夾 {}：{error}",
            parent.display()
        ))
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(StorageError::InvalidLifecycle(format!(
            "收藏路徑解析後不在目前設定的來源內：{}",
            current.path.display()
        )));
    }
    Ok(())
}

fn validate_archive_destination(
    transaction: &Transaction<'_>,
    archive_root_id: i64,
    destination: &Path,
    destination_must_exist: bool,
) -> StorageResult<ArchiveDestination> {
    if !destination
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        return Err(StorageError::InvalidLifecycle(
            "move 目標必須是 ZIP 檔案".to_owned(),
        ));
    }
    if destination_must_exist && !destination.is_file() {
        return Err(StorageError::InvalidLifecycle(format!(
            "move 目標 ZIP 不存在：{}",
            destination.display()
        )));
    }
    if !destination_must_exist && destination.exists() {
        return Err(StorageError::InvalidLifecycle(format!(
            "move 目標已存在，禁止覆寫：{}",
            destination.display()
        )));
    }
    let root = transaction
        .query_row(
            "SELECT path, source_kind, active FROM library_roots WHERE id = ?1",
            [archive_root_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::InvalidLifecycle(format!(
                "找不到 archive library root：{archive_root_id}"
            ))
        })?;
    if root.1 != "archive" || !root.2 {
        return Err(StorageError::InvalidLifecycle(
            "move 目標必須是啟用中的歸檔區".to_owned(),
        ));
    }
    let root_path = Path::new(&root.0);
    let relative_path = destination.strip_prefix(root_path).map_err(|_| {
        StorageError::InvalidLifecycle(format!(
            "move 目標不在指定歸檔區內：{}",
            destination.display()
        ))
    })?;
    let destination_parent = destination.parent().ok_or_else(|| {
        StorageError::InvalidLifecycle("move 目標沒有 parent directory".to_owned())
    })?;
    let canonical_root = fs::canonicalize(root_path).map_err(|error| {
        StorageError::InvalidLifecycle(format!(
            "無法解析歸檔區路徑 {}：{error}",
            root_path.display()
        ))
    })?;
    let canonical_parent = fs::canonicalize(destination_parent).map_err(|error| {
        StorageError::InvalidLifecycle(format!(
            "move 目標資料夾不存在或無法解析 {}：{error}",
            destination_parent.display()
        ))
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(StorageError::InvalidLifecycle(format!(
            "move 目標解析後不在指定歸檔區內：{}",
            destination.display()
        )));
    }
    let filename = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| StorageError::NonUnicodePath(destination.to_owned()))?;
    Ok(ArchiveDestination {
        relative_path: path_text(relative_path)?.to_owned(),
        filename: filename.to_owned(),
    })
}

fn pending_operation_row(
    transaction: &Transaction<'_>,
    operation_id: i64,
) -> StorageResult<PendingOperationRow> {
    let row = transaction
        .query_row(
            "SELECT collection_id, from_location_id, to_root_id, operation_kind,
                    from_path, to_path
             FROM file_operations WHERE id = ?1 AND status = 'pending'",
            [operation_id],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::InvalidLifecycle(format!(
                "file operation {operation_id} 不存在或不是 pending"
            ))
        })?;
    Ok(PendingOperationRow {
        collection_id: row.0.ok_or_else(|| {
            StorageError::InvalidLifecycle("pending operation 缺少 collection ID".to_owned())
        })?,
        from_location_id: row.1.ok_or_else(|| {
            StorageError::InvalidLifecycle("pending operation 缺少來源 location".to_owned())
        })?,
        to_root_id: row.2,
        kind: FileOperationKind::parse(&row.3).map_err(StorageError::InvalidSchema)?,
        from_path: PathBuf::from(row.4),
        to_path: row.5.map(PathBuf::from),
    })
}

fn complete_pending_move(
    transaction: &Transaction<'_>,
    operation: &PendingOperationRow,
) -> StorageResult<()> {
    if operation.from_path.exists() {
        return Err(StorageError::InvalidLifecycle(format!(
            "move 來源仍存在：{}",
            operation.from_path.display()
        )));
    }
    let destination = operation
        .to_path
        .as_ref()
        .ok_or_else(|| StorageError::InvalidLifecycle("pending move 缺少目標路徑".to_owned()))?;
    let archive_root_id = operation.to_root_id.ok_or_else(|| {
        StorageError::InvalidLifecycle("pending move 缺少 archive root".to_owned())
    })?;
    let destination_parts =
        validate_archive_destination(transaction, archive_root_id, destination, true)?;
    let destination_key = path_key(destination);
    let destination_conflict: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM collection_locations
             WHERE path_key = ?1 AND location_status = 'current'
               AND collection_id <> ?2
         )",
        params![destination_key, operation.collection_id],
        |row| row.get(0),
    )?;
    if destination_conflict {
        return Err(StorageError::InvalidLifecycle(format!(
            "move 目標已被其他收藏索引：{}",
            destination.display()
        )));
    }
    let changed = transaction.execute(
        "UPDATE collection_locations
         SET location_status = 'moved', ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ?1 AND collection_id = ?2 AND location_status = 'current'",
        params![operation.from_location_id, operation.collection_id],
    )?;
    if changed != 1 {
        return Err(StorageError::InvalidLifecycle(
            "move 來源 location 已變更".to_owned(),
        ));
    }
    transaction.execute(
        "INSERT INTO collection_locations(
             collection_id, root_id, full_path, path_key, relative_path,
             filename, location_status
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'current')",
        params![
            operation.collection_id,
            archive_root_id,
            path_text(destination)?,
            destination_key,
            destination_parts.relative_path,
            destination_parts.filename,
        ],
    )?;
    transaction.execute(
        "UPDATE collections
         SET status = 'active', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ?1",
        [operation.collection_id],
    )?;
    Ok(())
}

fn complete_pending_rename(
    transaction: &Transaction<'_>,
    operation: &PendingOperationRow,
) -> StorageResult<()> {
    if operation.from_path.exists() {
        return Err(StorageError::InvalidLifecycle(format!(
            "rename 來源仍存在：{}",
            operation.from_path.display()
        )));
    }
    let destination = operation
        .to_path
        .as_ref()
        .ok_or_else(|| StorageError::InvalidLifecycle("pending rename 缺少目標路徑".to_owned()))?;
    if !destination.is_file() {
        return Err(StorageError::InvalidLifecycle(format!(
            "rename 目標 ZIP 不存在：{}",
            destination.display()
        )));
    }
    let source_parent = operation.from_path.parent().ok_or_else(|| {
        StorageError::InvalidLifecycle("pending rename 來源缺少 parent directory".to_owned())
    })?;
    let destination_parent = destination.parent().ok_or_else(|| {
        StorageError::InvalidLifecycle("pending rename 目標缺少 parent directory".to_owned())
    })?;
    if path_key(source_parent) != path_key(destination_parent) {
        return Err(StorageError::InvalidLifecycle(
            "pending rename 目標已離開來源 parent directory".to_owned(),
        ));
    }
    let (root_id, root_path): (i64, String) = transaction.query_row(
        "SELECT location.root_id, root.path
         FROM collection_locations AS location
         JOIN library_roots AS root ON root.id = location.root_id
         WHERE location.id = ?1 AND location.collection_id = ?2",
        params![operation.from_location_id, operation.collection_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let root_path = Path::new(&root_path);
    let relative_path = destination.strip_prefix(root_path).map_err(|_| {
        StorageError::InvalidLifecycle(format!(
            "rename 目標不在原 library root 內：{}",
            destination.display()
        ))
    })?;
    let canonical_root = fs::canonicalize(root_path).map_err(|error| {
        StorageError::InvalidLifecycle(format!(
            "無法解析 rename library root {}：{error}",
            root_path.display()
        ))
    })?;
    let canonical_parent = fs::canonicalize(destination_parent).map_err(|error| {
        StorageError::InvalidLifecycle(format!(
            "無法解析 rename parent {}：{error}",
            destination_parent.display()
        ))
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(StorageError::InvalidLifecycle(format!(
            "rename 目標解析後不在原 library root 內：{}",
            destination.display()
        )));
    }
    let destination_key = path_key(destination);
    let destination_conflict: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM collection_locations
             WHERE path_key = ?1 AND location_status = 'current'
               AND collection_id <> ?2
         )",
        params![destination_key, operation.collection_id],
        |row| row.get(0),
    )?;
    if destination_conflict {
        return Err(StorageError::InvalidLifecycle(format!(
            "rename 目標已被其他收藏索引：{}",
            destination.display()
        )));
    }
    let changed = transaction.execute(
        "UPDATE collection_locations
         SET location_status = 'moved', ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ?1 AND collection_id = ?2 AND location_status = 'current'",
        params![operation.from_location_id, operation.collection_id],
    )?;
    if changed != 1 {
        return Err(StorageError::InvalidLifecycle(
            "rename 來源 location 已變更".to_owned(),
        ));
    }
    let filename = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| StorageError::NonUnicodePath(destination.to_owned()))?;
    transaction.execute(
        "INSERT INTO collection_locations(
             collection_id, root_id, full_path, path_key, relative_path,
             filename, location_status
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'current')",
        params![
            operation.collection_id,
            root_id,
            path_text(destination)?,
            destination_key,
            path_text(relative_path)?,
            filename,
        ],
    )?;
    transaction.execute(
        "UPDATE collections
         SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
        [operation.collection_id],
    )?;
    Ok(())
}

fn complete_pending_delete(
    transaction: &Transaction<'_>,
    operation: &PendingOperationRow,
    permanent: bool,
) -> StorageResult<()> {
    if operation.from_path.exists() {
        return Err(StorageError::InvalidLifecycle(format!(
            "刪除來源仍存在：{}",
            operation.from_path.display()
        )));
    }
    if permanent {
        let changed = transaction.execute(
            "DELETE FROM collections WHERE id = ?1",
            [operation.collection_id],
        )?;
        if changed != 1 {
            return Err(StorageError::CollectionNotFound(operation.collection_id));
        }
    } else {
        let changed = transaction.execute(
            "UPDATE collection_locations
             SET location_status = 'deleted',
                 ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1 AND collection_id = ?2 AND location_status = 'current'",
            params![operation.from_location_id, operation.collection_id],
        )?;
        if changed != 1 {
            return Err(StorageError::InvalidLifecycle(
                "刪除來源 location 已變更".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE collections
             SET status = 'soft_deleted', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [operation.collection_id],
        )?;
    }
    Ok(())
}

fn validated_canonical_name(name: &str) -> StorageResult<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(StorageError::InvalidCanonicalMapping(
            "canonical name 不得為空白".to_owned(),
        ));
    }
    Ok(name)
}

fn validated_raw_name(name: &str) -> StorageResult<&str> {
    if name.trim().is_empty() {
        return Err(StorageError::InvalidCanonicalMapping(
            "raw name 不得為空白".to_owned(),
        ));
    }
    Ok(name)
}

fn ensure_canonical_entity(transaction: &Transaction<'_>, entity_id: i64) -> StorageResult<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM canonical_entities WHERE id = ?1)",
        [entity_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(StorageError::CanonicalEntityNotFound(entity_id));
    }
    Ok(())
}

fn canonical_entity_kind(
    transaction: &Transaction<'_>,
    entity_id: i64,
) -> StorageResult<EntityKind> {
    let kind = transaction
        .query_row(
            "SELECT entity_kind FROM canonical_entities WHERE id = ?1",
            [entity_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StorageError::CanonicalEntityNotFound(entity_id))?;
    EntityKind::parse(&kind).map_err(StorageError::InvalidSchema)
}

fn mapped_collection_ids(transaction: &Transaction<'_>, entity_id: i64) -> StorageResult<Vec<i64>> {
    let mut statement = transaction.prepare(
        "SELECT DISTINCT assertion.collection_id
         FROM assertion_entities AS mapping
         JOIN metadata_assertions AS assertion ON assertion.id = mapping.assertion_id
         WHERE mapping.entity_id = ?1
         ORDER BY assertion.collection_id",
    )?;
    Ok(statement
        .query_map([entity_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn normalized_entity_pair(first: i64, second: i64) -> StorageResult<(i64, i64)> {
    if first == second {
        return Err(StorageError::InvalidCanonicalMapping(
            "同一個 entity 不能排除與自己合併".to_owned(),
        ));
    }
    Ok(if first < second {
        (first, second)
    } else {
        (second, first)
    })
}

fn validate_canonical_mapping(
    field: MetadataField,
    entity_kind: EntityKind,
    value_index: i64,
    raw_name: &str,
    value_json: &str,
) -> StorageResult<()> {
    use doujin_parser::domain::{Authors, Parody};

    let expected_kind = match field {
        MetadataField::Event => EntityKind::Event,
        MetadataField::Circle => EntityKind::Circle,
        MetadataField::Authors => EntityKind::Author,
        MetadataField::Parody => EntityKind::Parody,
        MetadataField::Title | MetadataField::Classification | MetadataField::IsDl => {
            return Err(StorageError::InvalidCanonicalMapping(format!(
                "{} 欄位不支援 canonical entity mapping",
                field.as_str()
            )));
        }
    };
    if entity_kind != expected_kind {
        return Err(StorageError::InvalidCanonicalMapping(format!(
            "{} 欄位不能映射到 {} entity",
            field.as_str(),
            entity_kind.as_str()
        )));
    }

    let index = usize::try_from(value_index).map_err(|_| {
        StorageError::InvalidCanonicalMapping("value index 超出支援範圍".to_owned())
    })?;
    let actual_raw = match field {
        MetadataField::Event | MetadataField::Circle => {
            if index != 0 {
                return Err(StorageError::InvalidCanonicalMapping(
                    "單值欄位的 value index 必須為 0".to_owned(),
                ));
            }
            serde_json::from_str::<String>(value_json)?
        }
        MetadataField::Authors => {
            let authors: Authors = serde_json::from_str(value_json)?;
            authors.values.get(index).cloned().ok_or_else(|| {
                StorageError::InvalidCanonicalMapping("作者 value index 不存在".to_owned())
            })?
        }
        MetadataField::Parody => {
            if index != 0 {
                return Err(StorageError::InvalidCanonicalMapping(
                    "單值欄位的 value index 必須為 0".to_owned(),
                ));
            }
            serde_json::from_str::<Parody>(value_json)?.raw
        }
        MetadataField::Title | MetadataField::Classification | MetadataField::IsDl => {
            unreachable!("unsupported fields returned above")
        }
    };
    if actual_raw != raw_name {
        return Err(StorageError::InvalidCanonicalMapping(format!(
            "raw name 與 assertion value 不一致：預期「{actual_raw}」"
        )));
    }
    Ok(())
}

fn canonical_name_for_assertion(
    transaction: &Transaction<'_>,
    assertion_id: i64,
    value_index: usize,
    field: MetadataField,
    raw_name: &str,
) -> StorageResult<Option<String>> {
    let value_index = i64::try_from(value_index).map_err(|_| {
        StorageError::InvalidCanonicalMapping("value index 超出支援範圍".to_owned())
    })?;
    let explicit = transaction
        .query_row(
            "SELECT entity.canonical_name
             FROM assertion_entities AS mapping
             JOIN canonical_entities AS entity ON entity.id = mapping.entity_id
             WHERE mapping.assertion_id = ?1 AND mapping.value_index = ?2",
            params![assertion_id, value_index],
            |row| row.get(0),
        )
        .optional()?;
    if explicit.is_some() {
        return Ok(explicit);
    }
    let vocabulary_field = match field {
        MetadataField::Event => "event",
        MetadataField::Circle => "circle",
        MetadataField::Authors => "author",
        MetadataField::Parody => "parody",
        MetadataField::Title | MetadataField::Classification | MetadataField::IsDl => {
            return Ok(None);
        }
    };
    Ok(transaction
        .query_row(
            "SELECT entity.canonical_name
             FROM vocabulary_aliases AS alias
             JOIN canonical_entities AS entity ON entity.id = alias.entity_id
             JOIN metadata_assertions AS assertion ON assertion.id = ?1
             WHERE alias.field_name = ?2 AND alias.alias = ?3
               AND assertion.source_kind <> 'manual' AND entity.status = 'active'",
            params![assertion_id, vocabulary_field, raw_name],
            |row| row.get(0),
        )
        .optional()?)
}

struct AssertionInsert<'a> {
    collection_id: i64,
    field: MetadataField,
    value_json: String,
    source_kind: &'a str,
    status: &'a str,
    parser_run_id: Option<i64>,
    source_reference: Option<&'a str>,
    confidence_total: Option<f64>,
    reason: Option<&'a str>,
}

fn insert_assertion(
    transaction: &Transaction<'_>,
    assertion: AssertionInsert<'_>,
) -> StorageResult<i64> {
    transaction.execute(
        "INSERT INTO metadata_assertions(
             collection_id, field_name, value_json, source_kind, status, parser_run_id,
             source_reference, confidence_total, reason
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            assertion.collection_id,
            assertion.field.as_str(),
            assertion.value_json,
            assertion.source_kind,
            assertion.status,
            assertion.parser_run_id,
            assertion.source_reference,
            assertion.confidence_total,
            assertion.reason,
        ],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn select_assertion(
    transaction: &Transaction<'_>,
    collection_id: i64,
    field: MetadataField,
    assertion_id: i64,
    selected_by: &str,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO metadata_selections(collection_id, field_name, assertion_id, selected_by)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(collection_id, field_name) DO UPDATE SET
             assertion_id = excluded.assertion_id,
             selected_by = excluded.selected_by,
             selected_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        params![collection_id, field.as_str(), assertion_id, selected_by],
    )?;
    Ok(())
}

fn has_protected_selection(
    transaction: &Transaction<'_>,
    collection_id: i64,
    field: MetadataField,
) -> StorageResult<bool> {
    Ok(transaction.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM metadata_selections AS selection
             JOIN metadata_assertions AS assertion ON assertion.id = selection.assertion_id
             WHERE selection.collection_id = ?1 AND selection.field_name = ?2
               AND (selection.selected_by IN ('manual', 'migration')
                    OR assertion.source_kind IN ('manual', 'legacy'))
         )",
        params![collection_id, field.as_str()],
        |row| row.get(0),
    )?)
}

fn matching_external_assertion(
    transaction: &Transaction<'_>,
    collection_id: i64,
    field: MetadataField,
    value_json: &str,
    source_reference: &str,
    confidence_total: f64,
    confidence_json: &str,
) -> StorageResult<Option<(i64, String)>> {
    Ok(transaction
        .query_row(
            "SELECT assertion.id, assertion.status
             FROM metadata_assertions AS assertion
             WHERE assertion.collection_id = ?1
               AND assertion.field_name = ?2
               AND assertion.source_kind = 'external'
               AND assertion.value_json = ?3
               AND assertion.source_reference = ?4
               AND assertion.confidence_total = ?5
               AND assertion.confidence_json = ?6
             ORDER BY EXISTS(
                 SELECT 1 FROM metadata_selections AS selection
                 WHERE selection.assertion_id = assertion.id
             ) DESC,
             CASE assertion.status
                 WHEN 'accepted' THEN 4
                 WHEN 'candidate' THEN 3
                 WHEN 'rejected' THEN 2
                 ELSE 1
             END DESC,
             assertion.id DESC
             LIMIT 1",
            params![
                collection_id,
                field.as_str(),
                value_json,
                source_reference,
                confidence_total,
                confidence_json,
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

fn reselect_by_priority(
    transaction: &Transaction<'_>,
    collection_id: i64,
    field: MetadataField,
    preserve_explicit_selection: bool,
) -> StorageResult<()> {
    if preserve_explicit_selection {
        let explicitly_selected: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM metadata_selections
                 WHERE collection_id = ?1 AND field_name = ?2
                   AND selected_by IN ('manual', 'migration')
             )",
            params![collection_id, field.as_str()],
            |row| row.get(0),
        )?;
        if explicitly_selected {
            return Ok(());
        }
    }

    let assertion_id = transaction
        .query_row(
            "SELECT id FROM metadata_assertions
             WHERE collection_id = ?1 AND field_name = ?2 AND status = 'accepted'
             ORDER BY CASE source_kind
                 WHEN 'manual' THEN 400
                 WHEN 'legacy' THEN 350
                 WHEN 'external' THEN 300
                 WHEN 'filename' THEN 200
                 WHEN 'inference' THEN 100
             END DESC, id DESC
             LIMIT 1",
            params![collection_id, field.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if let Some(assertion_id) = assertion_id {
        select_assertion(transaction, collection_id, field, assertion_id, "priority")?;
    } else {
        transaction.execute(
            "DELETE FROM metadata_selections WHERE collection_id = ?1 AND field_name = ?2",
            params![collection_id, field.as_str()],
        )?;
    }
    Ok(())
}

fn rebuild_projection(transaction: &Transaction<'_>, collection_id: i64) -> StorageResult<()> {
    use doujin_parser::domain::{Authors, Classification, Parody};

    let selections = {
        let mut statement = transaction.prepare(
            "SELECT assertion.id, selection.field_name, assertion.value_json
             FROM metadata_selections AS selection
             JOIN metadata_assertions AS assertion ON assertion.id = selection.assertion_id
             WHERE selection.collection_id = ?1",
        )?;
        statement
            .query_map([collection_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut title: Option<String> = None;
    let mut event: Option<String> = None;
    let mut circle: Option<String> = None;
    let mut authors = Authors {
        raw: None,
        values: Vec::new(),
    };
    let mut parody: Option<Parody> = None;
    let mut classification: Option<Classification> = None;
    let mut is_dl: Option<bool> = None;

    for (assertion_id, field_name, value_json) in selections {
        let decode_error = |error: serde_json::Error| StorageError::InvalidProjection {
            collection_id,
            reason: format!("{field_name} 的 JSON 無法解碼：{error}"),
        };
        match parse_field(&field_name)? {
            MetadataField::Title => {
                title = Some(serde_json::from_str(&value_json).map_err(decode_error)?)
            }
            MetadataField::Event => {
                let raw: String = serde_json::from_str(&value_json).map_err(decode_error)?;
                event = Some(
                    canonical_name_for_assertion(
                        transaction,
                        assertion_id,
                        0,
                        MetadataField::Event,
                        &raw,
                    )?
                    .unwrap_or(raw),
                );
            }
            MetadataField::Circle => {
                let raw: String = serde_json::from_str(&value_json).map_err(decode_error)?;
                circle = Some(
                    canonical_name_for_assertion(
                        transaction,
                        assertion_id,
                        0,
                        MetadataField::Circle,
                        &raw,
                    )?
                    .unwrap_or(raw),
                );
            }
            MetadataField::Authors => {
                authors = serde_json::from_str(&value_json).map_err(decode_error)?;
                for (index, author) in authors.values.iter_mut().enumerate() {
                    if let Some(canonical) = canonical_name_for_assertion(
                        transaction,
                        assertion_id,
                        index,
                        MetadataField::Authors,
                        author,
                    )? {
                        *author = canonical;
                    }
                }
            }
            MetadataField::Parody => {
                let mut value: Parody = serde_json::from_str(&value_json).map_err(decode_error)?;
                if let Some(canonical) = canonical_name_for_assertion(
                    transaction,
                    assertion_id,
                    0,
                    MetadataField::Parody,
                    &value.canonical,
                )? {
                    value.canonical = canonical;
                }
                parody = Some(value);
            }
            MetadataField::Classification => {
                classification = Some(serde_json::from_str(&value_json).map_err(decode_error)?)
            }
            MetadataField::IsDl => {
                is_dl = Some(serde_json::from_str(&value_json).map_err(decode_error)?)
            }
        }
    }

    let authors_text = authors.values.join("、");
    let authors_json = serde_json::to_string(&authors.values)?;
    let parody_canonical = parody.as_ref().map(|value| value.canonical.as_str());
    let parody_raw = parody.as_ref().map(|value| value.raw.as_str());
    let classification_top = classification
        .as_ref()
        .map(|value| value.top_level.as_str());
    let classification_subcategory = classification
        .as_ref()
        .and_then(|value| value.subcategory.as_deref());
    transaction.execute(
        "INSERT INTO effective_metadata(
             collection_id, title, event, circle, authors, authors_json, parody, parody_raw,
             classification_top, classification_subcategory, is_dl
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(collection_id) DO UPDATE SET
             title = excluded.title,
             event = excluded.event,
             circle = excluded.circle,
             authors = excluded.authors,
             authors_json = excluded.authors_json,
             parody = excluded.parody,
             parody_raw = excluded.parody_raw,
             classification_top = excluded.classification_top,
             classification_subcategory = excluded.classification_subcategory,
             is_dl = excluded.is_dl,
             projection_version = excluded.projection_version,
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        params![
            collection_id,
            title,
            event,
            circle,
            authors_text,
            authors_json,
            parody_canonical,
            parody_raw,
            classification_top,
            classification_subcategory,
            is_dl.map(i64::from),
        ],
    )?;
    Ok(())
}

fn parse_field(value: &str) -> StorageResult<MetadataField> {
    match value {
        "title" => Ok(MetadataField::Title),
        "event" => Ok(MetadataField::Event),
        "circle" => Ok(MetadataField::Circle),
        "authors" => Ok(MetadataField::Authors),
        "parody" => Ok(MetadataField::Parody),
        "classification" => Ok(MetadataField::Classification),
        "is_dl" => Ok(MetadataField::IsDl),
        _ => Err(StorageError::InvalidSchema(format!(
            "未知 metadata field：{value}"
        ))),
    }
}

fn ingest_one(
    transaction: &Transaction<'_>,
    pending: &PendingCollection,
) -> StorageResult<IngestOutcome> {
    let full_path = path_text(&pending.path)?;
    let current_path_key = path_key(&pending.path);
    let existing = transaction
        .query_row(
            "SELECT collection_id FROM collection_locations
             WHERE path_key = ?1 AND location_status = 'current'",
            [&current_path_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if existing.is_some() {
        return Ok(IngestOutcome::SkippedExisting);
    }

    let root_id = ensure_root(transaction, pending)?;
    transaction.execute(
        "INSERT INTO collections(status, media_kind, parser_version)
         VALUES ('active', 'zip', ?1)",
        [&pending.parser_version],
    )?;
    let collection_id = transaction.last_insert_rowid();

    let relative_path = pending.path.strip_prefix(&pending.root_path).map_err(|_| {
        StorageError::PathOutsideRoot {
            path: pending.path.clone(),
            root: pending.root_path.clone(),
        }
    })?;
    let filename = pending
        .path
        .file_name()
        .ok_or_else(|| StorageError::NonUnicodePath(pending.path.clone()))?;
    let filename = filename
        .to_str()
        .ok_or_else(|| StorageError::NonUnicodePath(pending.path.clone()))?;
    transaction.execute(
        "INSERT INTO collection_locations(
             collection_id, root_id, full_path, path_key, relative_path, filename, location_status
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'current')",
        params![
            collection_id,
            root_id,
            full_path,
            current_path_key,
            path_text(relative_path)?,
            filename,
        ],
    )?;

    let raw_filename_path = match &pending.filename_normalization {
        FilenameNormalization::Renamed { original, .. } => original,
        FilenameNormalization::Unchanged
        | FilenameNormalization::PlannedRename { .. }
        | FilenameNormalization::KeptOriginal { .. } => &pending.path,
    };
    let raw_filename = raw_filename_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| StorageError::NonUnicodePath(raw_filename_path.clone()))?;
    let result_json = serde_json::to_string(&pending.parsed)?;
    transaction.execute(
        "INSERT INTO parser_runs(collection_id, parser_version, raw_filename, result_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            collection_id,
            pending.parser_version,
            raw_filename,
            result_json
        ],
    )?;
    let parser_run_id = transaction.last_insert_rowid();

    select_filename_assertion(
        transaction,
        collection_id,
        parser_run_id,
        "title",
        serde_json::to_string(&pending.parsed.title)?,
    )?;
    if let Some(event) = &pending.parsed.event {
        select_filename_assertion(
            transaction,
            collection_id,
            parser_run_id,
            "event",
            serde_json::to_string(event)?,
        )?;
    }
    if let Some(circle) = &pending.parsed.circle {
        select_filename_assertion(
            transaction,
            collection_id,
            parser_run_id,
            "circle",
            serde_json::to_string(circle)?,
        )?;
    }
    if !pending.parsed.authors.values.is_empty() {
        select_filename_assertion(
            transaction,
            collection_id,
            parser_run_id,
            "authors",
            serde_json::to_string(&pending.parsed.authors)?,
        )?;
    }
    if let Some(parody) = &pending.parsed.parody {
        select_filename_assertion(
            transaction,
            collection_id,
            parser_run_id,
            "parody",
            serde_json::to_string(parody)?,
        )?;
    }
    select_filename_assertion(
        transaction,
        collection_id,
        parser_run_id,
        "classification",
        serde_json::to_string(&pending.parsed.classification)?,
    )?;
    select_filename_assertion(
        transaction,
        collection_id,
        parser_run_id,
        "is_dl",
        serde_json::to_string(&pending.parsed.is_dl)?,
    )?;

    rebuild_projection(transaction, collection_id)?;
    link_matching_tombstones(transaction, collection_id, filename)?;

    Ok(IngestOutcome::Inserted)
}

fn ensure_root(transaction: &Transaction<'_>, pending: &PendingCollection) -> StorageResult<i64> {
    upsert_library_root(
        transaction,
        &pending.root_path,
        pending.source,
        &pending.root_label,
    )
}

fn upsert_library_root(
    transaction: &Transaction<'_>,
    root_path: &Path,
    source: SourceKind,
    label: &str,
) -> StorageResult<i64> {
    let root_path_text = path_text(root_path)?;
    let root_path_key = path_key(root_path);
    transaction.execute(
        "INSERT INTO library_roots(path, path_key, source_kind, label)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(path_key) DO UPDATE SET
             path = excluded.path,
             source_kind = excluded.source_kind,
             label = excluded.label,
             active = 1,
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        params![
            root_path_text,
            root_path_key,
            source_kind_text(source),
            label
        ],
    )?;
    Ok(transaction.query_row(
        "SELECT id FROM library_roots WHERE path_key = ?1",
        [&root_path_key],
        |row| row.get(0),
    )?)
}

fn link_matching_tombstones(
    transaction: &Transaction<'_>,
    candidate_collection_id: i64,
    filename: &str,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO tombstone_candidates(
             tombstone_collection_id, candidate_collection_id, reason
         )
         SELECT DISTINCT collection.id, ?1, 'same_filename'
         FROM collections AS collection
         JOIN collection_locations AS location ON location.collection_id = collection.id
         WHERE collection.status = 'tombstone'
           AND location.filename = ?2 COLLATE NOCASE
           AND collection.id <> ?1
         ON CONFLICT(tombstone_collection_id, candidate_collection_id) DO NOTHING",
        params![candidate_collection_id, filename],
    )?;
    Ok(())
}

fn select_filename_assertion(
    transaction: &Transaction<'_>,
    collection_id: i64,
    parser_run_id: i64,
    field_name: &str,
    value_json: String,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO metadata_assertions(
             collection_id, field_name, value_json, source_kind, parser_run_id, status
         ) VALUES (?1, ?2, ?3, 'filename', ?4, 'accepted')",
        params![collection_id, field_name, value_json, parser_run_id],
    )?;
    let assertion_id = transaction.last_insert_rowid();
    transaction.execute(
        "INSERT INTO metadata_selections(collection_id, field_name, assertion_id, selected_by)
         VALUES (?1, ?2, ?3, 'priority')",
        params![collection_id, field_name, assertion_id],
    )?;
    Ok(())
}

fn source_kind_text(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Archive => "archive",
        SourceKind::Downloads => "downloads",
    }
}

fn path_text(path: &Path) -> StorageResult<&str> {
    path.to_str()
        .ok_or_else(|| StorageError::NonUnicodePath(path.to_owned()))
}

/// Produces a stable, case-insensitive key without requiring the path to exist.
pub fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.trim_end_matches('\\').to_lowercase()
}
