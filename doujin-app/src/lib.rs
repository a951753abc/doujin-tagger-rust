//! Application use cases over the scanner, repository, and file-operation service.

pub mod external_search;

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use doujin_files::{
    BatchReport, CollectionLaunchService, CollectionLauncher, DeleteRequest, FileOperationService,
    FileServiceError, LaunchError, LaunchReceipt, MoveRequest, RecycleBin,
    SystemCollectionLauncher, SystemRecycleBin,
};
use doujin_scanner::{ScanIssueKind, ScanRoot, SourceKind, scan_new_collections};
use doujin_storage::collections::{CollectionPage, CollectionQuery, CollectionSnapshot};
use doujin_storage::consolidation::{
    ConsolidationPreflight, ConsolidationResolution, ConsolidationSnapshot,
};
use doujin_storage::lifecycle::{CandidateDecision, TombstoneCandidateSnapshot};
use doujin_storage::metadata::{
    MetadataAssertionDecision, MetadataField, MetadataHistory, MetadataValue,
};
use doujin_storage::roots::LibraryRootSnapshot;
use doujin_storage::scan::{ScanCompletion, ScanCompletionStatus, ScanIssueRecord};
use doujin_storage::statistics::{CollectionFacet, CollectionStatistics, NamedCount};
use doujin_storage::thumbnails::{
    BACKGROUND_THUMBNAIL_PRIORITY, DEFAULT_THUMBNAIL_PRIORITY, ThumbnailRequestOutcome,
    ThumbnailStateSnapshot, ThumbnailStatus,
};
use doujin_storage::{CatalogRepository, IngestOutcome, StorageError};
use doujin_thumbnails::{
    ThumbnailConfig, ThumbnailError, ThumbnailGenerationRequest, ThumbnailGenerationSuccess,
    source_fingerprint,
};
use serde::Serialize;

#[derive(Debug)]
pub enum ApplicationError {
    Storage(StorageError),
    Json(serde_json::Error),
    Thumbnail(ThumbnailError),
    ThumbnailNotConfigured,
    ThumbnailCacheIo(std::io::Error),
    InvalidSettings(String),
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "scan summary JSON 錯誤：{error}"),
            Self::Thumbnail(error) => write!(formatter, "thumbnail 錯誤：{error}"),
            Self::ThumbnailNotConfigured => formatter.write_str("thumbnail 服務尚未設定"),
            Self::ThumbnailCacheIo(error) => write!(formatter, "thumbnail cache I/O 錯誤：{error}"),
            Self::InvalidSettings(reason) => {
                write!(formatter, "application settings 無效：{reason}")
            }
        }
    }
}

impl Error for ApplicationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Thumbnail(error) => Some(error),
            Self::ThumbnailNotConfigured => None,
            Self::ThumbnailCacheIo(error) => Some(error),
            Self::InvalidSettings(_) => None,
        }
    }
}

impl From<StorageError> for ApplicationError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<serde_json::Error> for ApplicationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<ThumbnailError> for ApplicationError {
    fn from(error: ThumbnailError) -> Self {
        Self::Thumbnail(error)
    }
}

pub type ApplicationResult<T> = Result<T, ApplicationError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationScanStatus {
    Succeeded,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationScanIssueKind {
    NoRoots,
    MissingRoot,
    ReadDirectory,
    ReadEntry,
    NonUnicodeFilename,
    Ingest,
    Reconcile,
}

impl ApplicationScanIssueKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::NoRoots => "no_roots",
            Self::MissingRoot => "missing_root",
            Self::ReadDirectory => "read_directory",
            Self::ReadEntry => "read_entry",
            Self::NonUnicodeFilename => "non_unicode_filename",
            Self::Ingest => "ingest",
            Self::Reconcile => "reconcile",
        }
    }
}

impl From<ScanIssueKind> for ApplicationScanIssueKind {
    fn from(kind: ScanIssueKind) -> Self {
        match kind {
            ScanIssueKind::NoRoots => Self::NoRoots,
            ScanIssueKind::MissingRoot => Self::MissingRoot,
            ScanIssueKind::ReadDirectory => Self::ReadDirectory,
            ScanIssueKind::ReadEntry => Self::ReadEntry,
            ScanIssueKind::NonUnicodeFilename => Self::NonUnicodeFilename,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationScanIssue {
    pub path: PathBuf,
    pub kind: ApplicationScanIssueKind,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplicationScanSummary {
    pub roots: usize,
    pub missing_roots: usize,
    pub discovered: usize,
    pub pending: usize,
    pub added: usize,
    pub skipped: usize,
    pub ingest_failed: usize,
    pub renamed: usize,
    pub normalization_warnings: usize,
    pub parse_complete: usize,
    pub parse_partial: usize,
    pub parse_title_only: usize,
    pub tombstoned: usize,
    pub candidate_links_created: usize,
    pub scan_elapsed_ms: u128,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationScanReport {
    pub scan_run_id: i64,
    pub status: ApplicationScanStatus,
    pub summary: ApplicationScanSummary,
    pub issues: Vec<ApplicationScanIssue>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplicationSettingsOverrides {
    pub reader_path: Option<PathBuf>,
    pub thumbnail_size: Option<(u32, u32)>,
    pub thumbnail_quality: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationSettingsSnapshot {
    pub reader_path: Option<PathBuf>,
    pub thumbnail_width: u32,
    pub thumbnail_height: u32,
    pub thumbnail_quality: u8,
    pub reader_overridden_by_environment: bool,
    pub thumbnail_size_overridden_by_environment: bool,
    pub thumbnail_quality_overridden_by_environment: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveSettingsOutcome {
    pub settings: ApplicationSettingsSnapshot,
    pub thumbnails_requeued: usize,
}

pub struct ApplicationService<R> {
    repository: CatalogRepository,
    recycle_bin: R,
    launcher: Box<dyn CollectionLauncher>,
    reader_path: Option<PathBuf>,
    thumbnail_config: Option<ThumbnailConfig>,
    settings_overrides: ApplicationSettingsOverrides,
}

impl ApplicationService<SystemRecycleBin> {
    pub fn with_system_recycle_bin(repository: CatalogRepository) -> Self {
        Self::new(repository, SystemRecycleBin)
    }

    pub fn with_system_recycle_bin_and_reader(
        repository: CatalogRepository,
        reader_path: Option<PathBuf>,
    ) -> Self {
        Self::with_launcher(
            repository,
            SystemRecycleBin,
            SystemCollectionLauncher,
            reader_path,
        )
    }

    pub fn with_system_services(
        repository: CatalogRepository,
        reader_path: Option<PathBuf>,
        thumbnail_config: ThumbnailConfig,
    ) -> Self {
        Self::with_launcher_and_thumbnails(
            repository,
            SystemRecycleBin,
            SystemCollectionLauncher,
            reader_path,
            thumbnail_config,
        )
    }

    pub fn with_system_services_and_overrides(
        repository: CatalogRepository,
        reader_path: Option<PathBuf>,
        thumbnail_config: ThumbnailConfig,
        settings_overrides: ApplicationSettingsOverrides,
    ) -> Self {
        Self::with_launcher_thumbnails_and_overrides(
            repository,
            SystemRecycleBin,
            SystemCollectionLauncher,
            reader_path,
            thumbnail_config,
            settings_overrides,
        )
    }
}

impl<R: RecycleBin> ApplicationService<R> {
    pub fn new(repository: CatalogRepository, recycle_bin: R) -> Self {
        Self::with_launcher(repository, recycle_bin, SystemCollectionLauncher, None)
    }

    pub fn with_thumbnails(
        repository: CatalogRepository,
        recycle_bin: R,
        thumbnail_config: ThumbnailConfig,
    ) -> Self {
        Self::with_launcher_and_thumbnails(
            repository,
            recycle_bin,
            SystemCollectionLauncher,
            None,
            thumbnail_config,
        )
    }

    pub fn with_launcher<L>(
        repository: CatalogRepository,
        recycle_bin: R,
        launcher: L,
        reader_path: Option<PathBuf>,
    ) -> Self
    where
        L: CollectionLauncher + 'static,
    {
        Self {
            repository,
            recycle_bin,
            launcher: Box::new(launcher),
            reader_path,
            thumbnail_config: None,
            settings_overrides: ApplicationSettingsOverrides::default(),
        }
    }

    pub fn with_launcher_and_thumbnails<L>(
        repository: CatalogRepository,
        recycle_bin: R,
        launcher: L,
        reader_path: Option<PathBuf>,
        thumbnail_config: ThumbnailConfig,
    ) -> Self
    where
        L: CollectionLauncher + 'static,
    {
        Self::with_launcher_thumbnails_and_overrides(
            repository,
            recycle_bin,
            launcher,
            reader_path,
            thumbnail_config,
            ApplicationSettingsOverrides::default(),
        )
    }

    pub fn with_launcher_thumbnails_and_overrides<L>(
        repository: CatalogRepository,
        recycle_bin: R,
        launcher: L,
        reader_path: Option<PathBuf>,
        thumbnail_config: ThumbnailConfig,
        settings_overrides: ApplicationSettingsOverrides,
    ) -> Self
    where
        L: CollectionLauncher + 'static,
    {
        Self {
            repository,
            recycle_bin,
            launcher: Box::new(launcher),
            reader_path,
            thumbnail_config: Some(thumbnail_config),
            settings_overrides,
        }
    }

    pub fn run_scan(&mut self, roots: &[ScanRoot]) -> ApplicationResult<ApplicationScanReport> {
        let started = Instant::now();
        let scan_run_id = self.repository.begin_scan_run()?;
        let existing_paths = match self.repository.current_paths() {
            Ok(paths) => paths,
            Err(error) => {
                return Err(fail_started_scan(
                    &mut self.repository,
                    scan_run_id,
                    roots.len(),
                    error,
                ));
            }
        };
        let candidate_links_before = match self.repository.tombstone_candidate_count() {
            Ok(count) => count,
            Err(error) => {
                return Err(fail_started_scan(
                    &mut self.repository,
                    scan_run_id,
                    roots.len(),
                    error,
                ));
            }
        };

        let scan_output = scan_new_collections(roots, &existing_paths);
        let safe_root_paths = safely_scanned_root_paths(roots, &scan_output.issues);
        let scanner_summary = scan_output.summary;
        let mut issues = scan_output
            .issues
            .into_iter()
            .map(|issue| ApplicationScanIssue {
                path: issue.path,
                kind: issue.kind.into(),
                message: issue.message,
            })
            .collect::<Vec<_>>();
        let mut added = 0_usize;
        let mut repository_skipped = 0_usize;
        let mut ingest_failed = 0_usize;
        for pending in scan_output.pending {
            match self.repository.ingest_collection(&pending) {
                Ok(IngestOutcome::Inserted) => added += 1,
                Ok(IngestOutcome::SkippedExisting) => repository_skipped += 1,
                Err(error) => {
                    ingest_failed += 1;
                    issues.push(ApplicationScanIssue {
                        path: pending.path,
                        kind: ApplicationScanIssueKind::Ingest,
                        message: error.to_string(),
                    });
                }
            }
        }

        let mut tombstoned = 0_usize;
        if !safe_root_paths.is_empty() {
            match self.repository.active_collection_locations() {
                Ok(locations) => {
                    for location in &locations {
                        if !safe_root_paths.contains(&location.root_path)
                            || !location.root_path.is_dir()
                            || location.path.exists()
                            || !has_existing_same_filename_candidate(location, &locations)
                        {
                            continue;
                        }
                        match self
                            .repository
                            .mark_collection_missing(location.collection_id)
                        {
                            Ok(()) => tombstoned += 1,
                            Err(_) if location.path.exists() => {}
                            Err(error) => issues.push(ApplicationScanIssue {
                                path: location.path.clone(),
                                kind: ApplicationScanIssueKind::Reconcile,
                                message: error.to_string(),
                            }),
                        }
                    }
                }
                Err(error) => issues.push(ApplicationScanIssue {
                    path: PathBuf::new(),
                    kind: ApplicationScanIssueKind::Reconcile,
                    message: error.to_string(),
                }),
            }
            if let Err(error) = self.repository.link_tombstones_to_active_same_filename() {
                issues.push(ApplicationScanIssue {
                    path: PathBuf::new(),
                    kind: ApplicationScanIssueKind::Reconcile,
                    message: error.to_string(),
                });
            }
        }
        let candidate_links_created = match self.repository.tombstone_candidate_count() {
            Ok(count) => count.saturating_sub(candidate_links_before),
            Err(error) => {
                issues.push(ApplicationScanIssue {
                    path: PathBuf::new(),
                    kind: ApplicationScanIssueKind::Reconcile,
                    message: error.to_string(),
                });
                0
            }
        };

        let summary = ApplicationScanSummary {
            roots: scanner_summary.roots,
            missing_roots: scanner_summary.missing_roots,
            discovered: scanner_summary.discovered,
            pending: scanner_summary.pending,
            added,
            skipped: scanner_summary.skipped_existing + repository_skipped,
            ingest_failed,
            renamed: scanner_summary.renamed,
            normalization_warnings: scanner_summary.normalization_warnings,
            parse_complete: scanner_summary.parse_complete,
            parse_partial: scanner_summary.parse_partial,
            parse_title_only: scanner_summary.parse_title_only,
            tombstoned,
            candidate_links_created,
            scan_elapsed_ms: scanner_summary.elapsed_ms,
            elapsed_ms: started.elapsed().as_millis(),
        };
        let status = if issues.is_empty() {
            ApplicationScanStatus::Succeeded
        } else {
            ApplicationScanStatus::Partial
        };
        let completion_status = match status {
            ApplicationScanStatus::Succeeded => ScanCompletionStatus::Succeeded,
            ApplicationScanStatus::Partial => ScanCompletionStatus::Partial,
        };
        let persisted_issues = issues
            .iter()
            .map(|issue| ScanIssueRecord {
                path: issue.path.to_string_lossy().into_owned(),
                kind: issue.kind.as_str().to_owned(),
                message: issue.message.clone(),
            })
            .collect();
        self.repository.complete_scan_run(
            scan_run_id,
            ScanCompletion {
                status: completion_status,
                summary_json: serde_json::to_string(&summary)?,
                issues: persisted_issues,
                error_message: None,
            },
        )?;
        Ok(ApplicationScanReport {
            scan_run_id,
            status,
            summary,
            issues,
        })
    }

    pub fn move_collections(&mut self, requests: &[MoveRequest]) -> BatchReport {
        FileOperationService::new(&mut self.repository, &self.recycle_bin).move_batch(requests)
    }

    pub fn move_collections_to_archive(
        &mut self,
        collection_ids: &[i64],
        archive_root_id: i64,
    ) -> BatchReport {
        FileOperationService::new(&mut self.repository, &self.recycle_bin)
            .move_to_archive_batch(collection_ids, archive_root_id)
    }

    pub fn delete_collections(&mut self, requests: &[DeleteRequest]) -> BatchReport {
        FileOperationService::new(&mut self.repository, &self.recycle_bin).delete_batch(requests)
    }

    pub fn recover_pending_file_operations(&mut self) -> Result<BatchReport, FileServiceError> {
        FileOperationService::new(&mut self.repository, &self.recycle_bin).recover_pending()
    }

    pub fn open_collection(&self, collection_id: i64) -> Result<LaunchReceipt, LaunchError> {
        CollectionLaunchService::new(&self.repository, self.launcher.as_ref())
            .open_default(collection_id)
    }

    pub fn read_collection(&self, collection_id: i64) -> Result<LaunchReceipt, LaunchError> {
        CollectionLaunchService::new(&self.repository, self.launcher.as_ref())
            .open_with_reader(collection_id, self.reader_path.as_deref())
    }

    pub fn library_roots(&self) -> ApplicationResult<Vec<LibraryRootSnapshot>> {
        Ok(self.repository.library_roots()?)
    }

    pub fn collections(&self, query: &CollectionQuery) -> ApplicationResult<CollectionPage> {
        Ok(self.repository.collections(query)?)
    }

    pub fn collection(&self, collection_id: i64) -> ApplicationResult<CollectionSnapshot> {
        Ok(self.repository.collection(collection_id)?)
    }

    pub fn metadata_history(&self, collection_id: i64) -> ApplicationResult<MetadataHistory> {
        self.repository.collection(collection_id)?;
        Ok(self.repository.metadata_history(collection_id)?)
    }

    pub fn decide_metadata_assertion(
        &mut self,
        collection_id: i64,
        field: MetadataField,
        assertion_id: i64,
        decision: MetadataAssertionDecision,
    ) -> ApplicationResult<MetadataHistory> {
        self.repository.collection(collection_id)?;
        self.repository
            .decide_metadata_assertion(collection_id, field, assertion_id, decision)?;
        Ok(self.repository.metadata_history(collection_id)?)
    }

    pub fn set_manual_metadata(
        &mut self,
        collection_id: i64,
        field: MetadataField,
        value: MetadataValue,
    ) -> ApplicationResult<CollectionSnapshot> {
        self.repository.collection(collection_id)?;
        self.repository
            .set_manual_value(collection_id, field, value)?;
        Ok(self.repository.collection(collection_id)?)
    }

    pub fn clear_manual_metadata(
        &mut self,
        collection_id: i64,
        field: MetadataField,
    ) -> ApplicationResult<CollectionSnapshot> {
        self.repository.collection(collection_id)?;
        self.repository.clear_manual_value(collection_id, field)?;
        Ok(self.repository.collection(collection_id)?)
    }

    pub fn add_collection_tag(
        &mut self,
        collection_id: i64,
        tag_name: &str,
    ) -> ApplicationResult<CollectionSnapshot> {
        self.repository.collection(collection_id)?;
        self.repository
            .add_collection_tag(collection_id, tag_name)?;
        Ok(self.repository.collection(collection_id)?)
    }

    pub fn remove_collection_tag(
        &mut self,
        collection_id: i64,
        tag_name: &str,
    ) -> ApplicationResult<CollectionSnapshot> {
        self.repository.collection(collection_id)?;
        self.repository
            .remove_collection_tag(collection_id, tag_name)?;
        Ok(self.repository.collection(collection_id)?)
    }

    pub fn register_library_root(
        &mut self,
        path: &Path,
        source: SourceKind,
        label: &str,
    ) -> ApplicationResult<LibraryRootSnapshot> {
        let root_id = self.repository.register_library_root(path, source, label)?;
        Ok(self.repository.library_root(root_id)?)
    }

    pub fn deactivate_library_root(
        &mut self,
        root_id: i64,
    ) -> ApplicationResult<LibraryRootSnapshot> {
        Ok(self.repository.deactivate_library_root(root_id)?)
    }

    pub fn tombstone_candidates(&self) -> ApplicationResult<Vec<TombstoneCandidateSnapshot>> {
        Ok(self.repository.all_tombstone_candidates()?)
    }

    pub fn decide_tombstone_candidate(
        &mut self,
        tombstone_collection_id: i64,
        candidate_collection_id: i64,
        decision: CandidateDecision,
    ) -> ApplicationResult<TombstoneCandidateSnapshot> {
        self.repository.decide_tombstone_candidate(
            tombstone_collection_id,
            candidate_collection_id,
            decision,
        )?;
        self.repository
            .tombstone_candidates(tombstone_collection_id)?
            .into_iter()
            .find(|candidate| candidate.candidate_collection_id == candidate_collection_id)
            .ok_or_else(|| {
                StorageError::InvalidLifecycle("tombstone candidate 裁決後無法讀回".to_owned())
                    .into()
            })
    }

    pub fn consolidation_preflight(
        &self,
        tombstone_collection_id: i64,
        candidate_collection_id: i64,
    ) -> ApplicationResult<ConsolidationPreflight> {
        Ok(self
            .repository
            .consolidation_preflight(tombstone_collection_id, candidate_collection_id)?)
    }

    pub fn consolidate_tombstone_candidate(
        &mut self,
        tombstone_collection_id: i64,
        candidate_collection_id: i64,
        resolutions: &[ConsolidationResolution],
    ) -> ApplicationResult<ConsolidationSnapshot> {
        Ok(self.repository.consolidate_tombstone_candidate(
            tombstone_collection_id,
            candidate_collection_id,
            resolutions,
        )?)
    }

    pub fn merged_into_collection(&self, collection_id: i64) -> ApplicationResult<Option<i64>> {
        Ok(self.repository.merged_into_collection(collection_id)?)
    }

    pub fn application_settings(&self) -> ApplicationResult<ApplicationSettingsSnapshot> {
        let config = self.thumbnail_config()?;
        Ok(ApplicationSettingsSnapshot {
            reader_path: self.reader_path.clone(),
            thumbnail_width: config.width,
            thumbnail_height: config.height,
            thumbnail_quality: config.quality,
            reader_overridden_by_environment: self.settings_overrides.reader_path.is_some(),
            thumbnail_size_overridden_by_environment: self
                .settings_overrides
                .thumbnail_size
                .is_some(),
            thumbnail_quality_overridden_by_environment: self
                .settings_overrides
                .thumbnail_quality
                .is_some(),
        })
    }

    pub fn save_application_settings(
        &mut self,
        reader_path: Option<PathBuf>,
        thumbnail_width: u32,
        thumbnail_height: u32,
        thumbnail_quality: u8,
    ) -> ApplicationResult<SaveSettingsOutcome> {
        if reader_path
            .as_deref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(ApplicationError::InvalidSettings(
                "reader path 必須是絕對路徑".to_owned(),
            ));
        }
        let cache_dir = self.thumbnail_config()?.cache_dir.clone();
        ThumbnailConfig::new(
            cache_dir.clone(),
            thumbnail_width,
            thumbnail_height,
            thumbnail_quality,
        )
        .map_err(|error| ApplicationError::InvalidSettings(error.to_string()))?;
        let effective_reader = self
            .settings_overrides
            .reader_path
            .clone()
            .or_else(|| reader_path.clone());
        let (effective_width, effective_height) = self
            .settings_overrides
            .thumbnail_size
            .unwrap_or((thumbnail_width, thumbnail_height));
        let effective_quality = self
            .settings_overrides
            .thumbnail_quality
            .unwrap_or(thumbnail_quality);
        let effective_thumbnail = ThumbnailConfig::new(
            cache_dir,
            effective_width,
            effective_height,
            effective_quality,
        )
        .map_err(|error| ApplicationError::InvalidSettings(error.to_string()))?;
        let saved = self.repository.save_application_settings(
            reader_path.as_deref(),
            thumbnail_width,
            thumbnail_height,
            thumbnail_quality,
            &effective_thumbnail.settings_fingerprint(),
        )?;
        self.reader_path = effective_reader;
        self.thumbnail_config = Some(effective_thumbnail);
        Ok(SaveSettingsOutcome {
            settings: self.application_settings()?,
            thumbnails_requeued: saved.thumbnails_requeued,
        })
    }

    pub fn collection_statistics(&self) -> ApplicationResult<CollectionStatistics> {
        Ok(self.repository.collection_statistics()?)
    }

    pub fn collection_facets(
        &self,
        facet: CollectionFacet,
        search: &str,
        limit: u32,
    ) -> ApplicationResult<Vec<NamedCount>> {
        Ok(self.repository.collection_facets(facet, search, limit)?)
    }

    pub fn request_thumbnail(
        &mut self,
        collection_id: i64,
    ) -> ApplicationResult<ThumbnailRequestOutcome> {
        self.request_thumbnail_with_priority(collection_id, DEFAULT_THUMBNAIL_PRIORITY)
    }

    pub fn request_thumbnail_with_priority(
        &mut self,
        collection_id: i64,
        priority: i64,
    ) -> ApplicationResult<ThumbnailRequestOutcome> {
        self.repository.collection(collection_id)?;
        let config = self.thumbnail_config()?;
        let source_path = self.repository.active_collection_file_path(collection_id)?;
        let source_fingerprint = source_fingerprint(&source_path)?;
        let cache_path = config.cache_path(collection_id);
        Ok(self.repository.request_thumbnail_with_priority(
            collection_id,
            &source_fingerprint,
            &config.settings_fingerprint(),
            &cache_path,
            cache_path.is_file(),
            priority,
        )?)
    }

    pub fn read_thumbnail_cache(&self, collection_id: i64) -> ApplicationResult<Option<Vec<u8>>> {
        let config = self.thumbnail_config()?;
        let state = self.repository.thumbnail_state(collection_id)?;
        let expected_path = config.cache_path(collection_id);
        if state.status != ThumbnailStatus::Ready || state.cache_path != expected_path {
            return Ok(None);
        }
        Ok(Some(
            fs::read(expected_path).map_err(ApplicationError::ThumbnailCacheIo)?,
        ))
    }

    pub fn due_thumbnails(&self, limit: u32) -> ApplicationResult<Vec<ThumbnailStateSnapshot>> {
        Ok(self.repository.due_thumbnails(limit)?)
    }

    pub fn due_thumbnails_with_min_priority(
        &self,
        limit: u32,
        min_priority: i64,
    ) -> ApplicationResult<Vec<ThumbnailStateSnapshot>> {
        Ok(self
            .repository
            .due_thumbnails_with_min_priority(limit, min_priority)?)
    }

    pub fn prewarm_next_thumbnail(&mut self) -> ApplicationResult<Option<ThumbnailRequestOutcome>> {
        let Some(collection_id) = self.repository.next_untracked_thumbnail_collection_id()? else {
            return Ok(None);
        };
        self.request_thumbnail_with_priority(collection_id, BACKGROUND_THUMBNAIL_PRIORITY)
            .map(Some)
    }

    pub fn start_thumbnail_generation(
        &mut self,
        collection_id: i64,
    ) -> ApplicationResult<ThumbnailGenerationRequest> {
        let config = self.thumbnail_config()?.clone();
        let source_path = self.repository.active_collection_file_path(collection_id)?;
        let state = self.repository.start_thumbnail(collection_id)?;
        Ok(ThumbnailGenerationRequest {
            source_path,
            cache_path: state.cache_path,
            width: config.width,
            height: config.height,
            quality: config.quality,
        })
    }

    pub fn finish_thumbnail_generation(
        &mut self,
        collection_id: i64,
        result: Result<ThumbnailGenerationSuccess, ThumbnailError>,
    ) -> ApplicationResult<ThumbnailStateSnapshot> {
        let current = self.repository.thumbnail_state(collection_id)?;
        if current.status != ThumbnailStatus::Running {
            return Ok(current);
        }
        Ok(match result {
            Ok(success) => {
                self.repository
                    .complete_thumbnail(collection_id, success.width, success.height)?
            }
            Err(error) => {
                self.repository
                    .fail_thumbnail(collection_id, error.kind, &error.message)?
            }
        })
    }

    pub fn recover_interrupted_thumbnails(&mut self) -> ApplicationResult<usize> {
        Ok(self.repository.recover_interrupted_thumbnails()?)
    }

    pub fn rebuild_thumbnail(
        &mut self,
        collection_id: i64,
    ) -> ApplicationResult<ThumbnailStateSnapshot> {
        self.repository.collection(collection_id)?;
        let config = self.thumbnail_config()?.clone();
        let source_path = self.repository.active_collection_file_path(collection_id)?;
        let source_fingerprint = source_fingerprint(&source_path)?;
        remove_cache_if_present(&config.cache_path(collection_id))?;
        Ok(self.repository.reset_thumbnail(
            collection_id,
            &source_fingerprint,
            &config.settings_fingerprint(),
            &config.cache_path(collection_id),
        )?)
    }

    pub fn rebuild_all_thumbnails(&mut self) -> ApplicationResult<usize> {
        let collection_ids = self.repository.active_collection_ids()?;
        let mut rebuilt = 0;
        for collection_id in collection_ids {
            self.rebuild_thumbnail(collection_id)?;
            rebuilt += 1;
        }
        Ok(rebuilt)
    }

    pub fn reconcile_thumbnail_settings(&mut self) -> ApplicationResult<usize> {
        let collection_ids = self.repository.thumbnail_collection_ids()?;
        let mut enqueued = 0;
        for collection_id in collection_ids {
            if self.request_thumbnail(collection_id)?.enqueued {
                enqueued += 1;
            }
        }
        Ok(enqueued)
    }

    pub fn repository(&self) -> &CatalogRepository {
        &self.repository
    }

    pub fn into_repository(self) -> CatalogRepository {
        self.repository
    }

    fn thumbnail_config(&self) -> ApplicationResult<&ThumbnailConfig> {
        self.thumbnail_config
            .as_ref()
            .ok_or(ApplicationError::ThumbnailNotConfigured)
    }
}

fn remove_cache_if_present(path: &Path) -> ApplicationResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ApplicationError::ThumbnailCacheIo(error)),
    }
}

fn fail_started_scan(
    repository: &mut CatalogRepository,
    scan_run_id: i64,
    roots: usize,
    error: StorageError,
) -> ApplicationError {
    let message = error.to_string();
    let _ = repository.complete_scan_run(
        scan_run_id,
        ScanCompletion {
            status: ScanCompletionStatus::Failed,
            summary_json: serde_json::json!({ "roots": roots }).to_string(),
            issues: Vec::new(),
            error_message: Some(message),
        },
    );
    ApplicationError::Storage(error)
}

fn safely_scanned_root_paths(
    roots: &[ScanRoot],
    issues: &[doujin_scanner::ScanIssue],
) -> HashSet<PathBuf> {
    roots
        .iter()
        .filter(|root| root.path.is_dir())
        .filter(|root| {
            !issues.iter().any(|issue| {
                matches!(
                    issue.kind,
                    ScanIssueKind::MissingRoot
                        | ScanIssueKind::ReadDirectory
                        | ScanIssueKind::ReadEntry
                ) && issue.path.starts_with(&root.path)
            })
        })
        .map(|root| root.path.clone())
        .collect()
}

fn has_existing_same_filename_candidate(
    missing: &doujin_storage::lifecycle::ActiveCollectionLocationSnapshot,
    locations: &[doujin_storage::lifecycle::ActiveCollectionLocationSnapshot],
) -> bool {
    let Some(filename) = missing.path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    locations.iter().any(|candidate| {
        candidate.collection_id != missing.collection_id
            && candidate.path.is_file()
            && candidate
                .path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(filename))
    })
}
