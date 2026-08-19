//! Application use cases over the scanner, repository, and file-operation service.

pub mod archive;
pub mod duplicates;
pub mod export;
pub mod external_search;
pub mod rename;

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use doujin_files::{
    BatchReport, CollectionLaunchService, CollectionLauncher, DeleteRequest, FileOperationService,
    FileServiceError, LaunchError, LaunchReceipt, MoveRequest, RecycleBin, RenameRequest,
    SystemCollectionLauncher, SystemRecycleBin,
};
use doujin_scanner::{
    FilenameNormalization, ScanIssueKind, ScanMode, ScanRoot, SourceKind,
    scan_new_collections_with_mode,
};
use doujin_storage::collections::{
    CollectionPage, CollectionQuery, CollectionQueryLocation, CollectionSnapshot, ReviewQueuePage,
    ReviewQueueQuery,
};
use doujin_storage::consolidation::{
    ConsolidationPreflight, ConsolidationResolution, ConsolidationSnapshot,
};
use doujin_storage::covers::{CoverSelectionSnapshot, CoverSelectionStatus};
use doujin_storage::lifecycle::{CandidateDecision, TombstoneCandidateSnapshot};
use doujin_storage::metadata::{
    MetadataAssertionDecision, MetadataField, MetadataHistory, MetadataValue,
};
use doujin_storage::roots::LibraryRootSnapshot;
use doujin_storage::saved_views::{SavedViewQuery, SavedViewSnapshot};
use doujin_storage::scan::{ScanCompletion, ScanCompletionStatus, ScanIssueRecord};
use doujin_storage::settings::DEFAULT_LIBRARY_BATCH_SIZE;
use doujin_storage::shelf_composition::ShelfConfigurationItem;
use doujin_storage::statistics::{CollectionFacet, CollectionStatistics, NamedCount};
use doujin_storage::thumbnails::{
    BACKGROUND_THUMBNAIL_PRIORITY, BATCH_THUMBNAIL_PRIORITY, DEFAULT_THUMBNAIL_PRIORITY,
    ThumbnailErrorKind, ThumbnailRequestOutcome, ThumbnailStateSnapshot, ThumbnailStatus,
    ThumbnailStatusCounts,
};
use doujin_storage::vocabulary::{
    VocabularyCandidateGroup, VocabularyField, VocabularyMergePreflight, VocabularyMergeResult,
    VocabularySuggestion,
};
use doujin_storage::work_baskets::{WorkBasketSnapshot, WorkBasketSummary};
use doujin_storage::{CatalogRepository, IngestOutcome, StorageError};
use doujin_thumbnails::{
    CoverCandidate, ThumbnailConfig, ThumbnailError, ThumbnailGenerationRequest,
    ThumbnailGenerationSuccess, cover_candidate_preview, cover_candidates,
    cover_source_fingerprint, source_fingerprint, validate_cover_candidate,
};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum ApplicationError {
    Storage(StorageError),
    Json(serde_json::Error),
    Thumbnail(ThumbnailError),
    ThumbnailNotConfigured,
    ThumbnailCacheIo(std::io::Error),
    ExportIo(std::io::Error),
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
            Self::ExportIo(error) => write!(formatter, "export I/O 錯誤：{error}"),
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
            Self::ExportIo(error) => Some(error),
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

#[derive(Debug)]
pub enum ApplicationBatchOutcome {
    Succeeded(CollectionSnapshot),
    Unchanged(CollectionSnapshot),
    Failed(ApplicationError),
}

#[derive(Debug)]
pub struct ApplicationBatchItem {
    pub collection_id: i64,
    pub outcome: ApplicationBatchOutcome,
}

#[derive(Debug)]
pub struct ApplicationBatchReport {
    pub items: Vec<ApplicationBatchItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedViewWithCount {
    pub view: SavedViewSnapshot,
    pub result_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailCachePreparation {
    pub collection_ids: Vec<i64>,
    pub failed_collection_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailCachePreflight {
    pub root_ids: Vec<i64>,
    pub collection_ids: Vec<i64>,
    pub ready: usize,
}

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
    pub planned_renames: usize,
    pub normalization_warnings: usize,
    pub parse_complete: usize,
    pub parse_partial: usize,
    pub parse_title_only: usize,
    pub tombstoned: usize,
    pub candidate_links_created: usize,
    pub scan_elapsed_ms: u128,
    pub elapsed_ms: u128,
    pub preflight_differences: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationScanReport {
    pub scan_run_id: i64,
    pub status: ApplicationScanStatus,
    pub summary: ApplicationScanSummary,
    pub issues: Vec<ApplicationScanIssue>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ApplicationScanMode {
    #[default]
    ApplySafeRenames,
    NoRename,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationScanExpectation {
    pub discovered: usize,
    pub new_collections: usize,
    pub already_known: usize,
    pub planned_renames: usize,
    pub normalization_warnings: usize,
    pub possible_tombstones: usize,
    pub possible_candidate_links: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplicationScanOptions {
    pub mode: ApplicationScanMode,
    pub expected: Option<ApplicationScanExpectation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationScanRootScope {
    pub path: PathBuf,
    pub label: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationPlannedRename {
    pub before: PathBuf,
    pub after: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationRenameWarning {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationTombstonePreflightCandidate {
    pub tombstone_collection_id: i64,
    pub tombstone_path: PathBuf,
    pub candidate_collection_id: Option<i64>,
    pub candidate_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationScanPreflight {
    pub roots: Vec<ApplicationScanRootScope>,
    pub expectation: ApplicationScanExpectation,
    pub renames: Vec<ApplicationPlannedRename>,
    pub rename_warnings: Vec<ApplicationRenameWarning>,
    pub tombstone_candidates: Vec<ApplicationTombstonePreflightCandidate>,
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
    pub saved_reader_path: Option<PathBuf>,
    pub saved_thumbnail_width: u32,
    pub saved_thumbnail_height: u32,
    pub saved_thumbnail_quality: u8,
    pub reader_overridden_by_environment: bool,
    pub thumbnail_size_overridden_by_environment: bool,
    pub thumbnail_quality_overridden_by_environment: bool,
    pub default_archive_root_id: Option<i64>,
    pub library_batch_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveSettingsOutcome {
    pub settings: ApplicationSettingsSnapshot,
    pub thumbnails_requeued: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationCoverSelection {
    pub entry_path: String,
    pub source_fingerprint: String,
    pub status: String,
    pub error: Option<String>,
    pub selected_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationCoverCandidate {
    pub entry_path: String,
    pub filename: String,
    pub page_order: usize,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationCoverCandidates {
    pub source_fingerprint: String,
    pub selection: Option<ApplicationCoverSelection>,
    pub items: Vec<ApplicationCoverCandidate>,
}

impl From<CoverSelectionSnapshot> for ApplicationCoverSelection {
    fn from(selection: CoverSelectionSnapshot) -> Self {
        Self {
            entry_path: selection.entry_path,
            source_fingerprint: selection.source_fingerprint,
            status: selection.validation_status.as_str().to_owned(),
            error: selection.validation_error,
            selected_at: selection.selected_at,
        }
    }
}

impl From<CoverCandidate> for ApplicationCoverCandidate {
    fn from(candidate: CoverCandidate) -> Self {
        Self {
            entry_path: candidate.entry_path,
            filename: candidate.filename,
            page_order: candidate.page_order,
            width: candidate.width,
            height: candidate.height,
        }
    }
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

    pub fn preflight_scan(
        &self,
        roots: &[ScanRoot],
    ) -> ApplicationResult<ApplicationScanPreflight> {
        let existing_paths = self.repository.current_paths()?;
        let locations = self.repository.active_collection_locations()?;
        let scan_output = scan_new_collections_with_mode(roots, &existing_paths, ScanMode::DryRun);
        let safe_root_paths = safely_scanned_root_paths(roots, &scan_output.issues);
        let mut renames = Vec::new();
        let mut rename_warnings = Vec::new();
        for pending in &scan_output.pending {
            match &pending.filename_normalization {
                FilenameNormalization::PlannedRename { original, renamed } => {
                    renames.push(ApplicationPlannedRename {
                        before: original.clone(),
                        after: renamed.clone(),
                        reason: "percent_decode_and_structural_parse".to_owned(),
                    });
                }
                FilenameNormalization::KeptOriginal { reason } => {
                    rename_warnings.push(ApplicationRenameWarning {
                        path: pending.path.clone(),
                        reason: reason.clone(),
                    });
                }
                FilenameNormalization::Unchanged | FilenameNormalization::Renamed { .. } => {}
            }
        }

        let anticipated_paths = scan_output
            .pending
            .iter()
            .map(|pending| match &pending.filename_normalization {
                FilenameNormalization::PlannedRename { renamed, .. } => renamed.clone(),
                _ => pending.path.clone(),
            })
            .collect::<Vec<_>>();
        let mut tombstone_candidates = Vec::new();
        for missing in &locations {
            if !safe_root_paths.contains(&missing.root_path)
                || !missing.root_path.is_dir()
                || missing.path.exists()
            {
                continue;
            }
            let Some(filename) = missing.path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            for candidate in &locations {
                if candidate.collection_id == missing.collection_id
                    || !candidate.path.is_file()
                    || !same_filename(&candidate.path, filename)
                {
                    continue;
                }
                tombstone_candidates.push(ApplicationTombstonePreflightCandidate {
                    tombstone_collection_id: missing.collection_id,
                    tombstone_path: missing.path.clone(),
                    candidate_collection_id: Some(candidate.collection_id),
                    candidate_path: candidate.path.clone(),
                });
            }
            for candidate_path in &anticipated_paths {
                if !same_filename(candidate_path, filename) {
                    continue;
                }
                tombstone_candidates.push(ApplicationTombstonePreflightCandidate {
                    tombstone_collection_id: missing.collection_id,
                    tombstone_path: missing.path.clone(),
                    candidate_collection_id: None,
                    candidate_path: candidate_path.clone(),
                });
            }
        }
        tombstone_candidates.sort_by(|left, right| {
            left.tombstone_path
                .cmp(&right.tombstone_path)
                .then_with(|| left.candidate_path.cmp(&right.candidate_path))
        });
        let possible_tombstones = tombstone_candidates
            .iter()
            .map(|candidate| candidate.tombstone_collection_id)
            .collect::<HashSet<_>>()
            .len();
        let expectation = ApplicationScanExpectation {
            discovered: scan_output.summary.discovered,
            new_collections: scan_output.summary.pending,
            already_known: scan_output.summary.skipped_existing,
            planned_renames: scan_output.summary.planned_renames,
            normalization_warnings: scan_output.summary.normalization_warnings,
            possible_tombstones,
            possible_candidate_links: tombstone_candidates.len(),
        };
        Ok(ApplicationScanPreflight {
            roots: roots
                .iter()
                .map(|root| ApplicationScanRootScope {
                    path: root.path.clone(),
                    label: root.label.clone(),
                    source: match root.source {
                        SourceKind::Archive => "archive",
                        SourceKind::Downloads => "downloads",
                    }
                    .to_owned(),
                })
                .collect(),
            expectation,
            renames,
            rename_warnings,
            tombstone_candidates,
            issues: scan_output
                .issues
                .into_iter()
                .map(|issue| ApplicationScanIssue {
                    path: issue.path,
                    kind: issue.kind.into(),
                    message: issue.message,
                })
                .collect(),
        })
    }

    pub fn run_scan(&mut self, roots: &[ScanRoot]) -> ApplicationResult<ApplicationScanReport> {
        self.run_scan_with_options(roots, ApplicationScanOptions::default())
    }

    pub fn run_scan_with_options(
        &mut self,
        roots: &[ScanRoot],
        options: ApplicationScanOptions,
    ) -> ApplicationResult<ApplicationScanReport> {
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

        let scan_output = scan_new_collections_with_mode(
            roots,
            &existing_paths,
            match options.mode {
                ApplicationScanMode::ApplySafeRenames => ScanMode::ApplyRenames,
                ApplicationScanMode::NoRename => ScanMode::NoRename,
            },
        );
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

        let mut summary = ApplicationScanSummary {
            roots: scanner_summary.roots,
            missing_roots: scanner_summary.missing_roots,
            discovered: scanner_summary.discovered,
            pending: scanner_summary.pending,
            added,
            skipped: scanner_summary.skipped_existing + repository_skipped,
            ingest_failed,
            renamed: scanner_summary.renamed,
            planned_renames: scanner_summary.planned_renames,
            normalization_warnings: scanner_summary.normalization_warnings,
            parse_complete: scanner_summary.parse_complete,
            parse_partial: scanner_summary.parse_partial,
            parse_title_only: scanner_summary.parse_title_only,
            tombstoned,
            candidate_links_created,
            scan_elapsed_ms: scanner_summary.elapsed_ms,
            elapsed_ms: started.elapsed().as_millis(),
            preflight_differences: Vec::new(),
        };
        if let Some(expected) = options.expected {
            record_preflight_difference(
                &mut summary.preflight_differences,
                "發現項目",
                expected.discovered,
                summary.discovered,
            );
            record_preflight_difference(
                &mut summary.preflight_differences,
                "預計新增",
                expected.new_collections,
                summary.pending,
            );
            record_preflight_difference(
                &mut summary.preflight_differences,
                "已知／略過",
                expected.already_known,
                summary.skipped,
            );
            let expected_renames = match options.mode {
                ApplicationScanMode::ApplySafeRenames => expected.planned_renames,
                ApplicationScanMode::NoRename => 0,
            };
            record_preflight_difference(
                &mut summary.preflight_differences,
                "實體改名",
                expected_renames,
                summary.renamed,
            );
            record_preflight_difference(
                &mut summary.preflight_differences,
                "改名警告",
                expected.normalization_warnings,
                summary.normalization_warnings,
            );
            record_preflight_difference(
                &mut summary.preflight_differences,
                "tombstone",
                expected.possible_tombstones,
                summary.tombstoned,
            );
            record_preflight_difference(
                &mut summary.preflight_differences,
                "身分候選",
                expected.possible_candidate_links,
                summary.candidate_links_created,
            );
        }
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

    pub fn rename_collections(&mut self, requests: &[RenameRequest]) -> BatchReport {
        FileOperationService::new(&mut self.repository, &self.recycle_bin).rename_batch(requests)
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

    pub fn review_queue(&self, query: &ReviewQueueQuery) -> ApplicationResult<ReviewQueuePage> {
        Ok(self.repository.review_queue(query)?)
    }

    pub fn work_baskets(&self) -> ApplicationResult<Vec<WorkBasketSummary>> {
        Ok(self.repository.work_baskets()?)
    }

    pub fn work_basket(&self, basket_id: i64) -> ApplicationResult<WorkBasketSnapshot> {
        Ok(self.repository.work_basket(basket_id)?)
    }

    pub fn add_to_work_basket(
        &mut self,
        basket_id: i64,
        collection_ids: &[i64],
    ) -> ApplicationResult<WorkBasketSnapshot> {
        Ok(self
            .repository
            .add_to_work_basket(basket_id, collection_ids)?)
    }

    pub fn remove_from_work_basket(
        &mut self,
        basket_id: i64,
        collection_id: i64,
    ) -> ApplicationResult<WorkBasketSnapshot> {
        self.repository
            .remove_from_work_basket(basket_id, collection_id)?;
        Ok(self.repository.work_basket(basket_id)?)
    }

    pub fn clear_work_basket(&mut self, basket_id: i64) -> ApplicationResult<WorkBasketSnapshot> {
        self.repository.clear_work_basket(basket_id)?;
        Ok(self.repository.work_basket(basket_id)?)
    }

    pub fn saved_views(&self) -> ApplicationResult<Vec<SavedViewWithCount>> {
        self.repository
            .saved_views()?
            .into_iter()
            .map(|view| self.saved_view_with_count(view))
            .collect()
    }

    pub fn saved_view(&self, saved_view_id: i64) -> ApplicationResult<SavedViewWithCount> {
        let view = self.repository.saved_view(saved_view_id)?;
        self.saved_view_with_count(view)
    }

    pub fn create_saved_view(
        &mut self,
        name: &str,
        query: &SavedViewQuery,
        pinned: bool,
    ) -> ApplicationResult<SavedViewWithCount> {
        let view = self.repository.create_saved_view(name, query, pinned)?;
        self.saved_view_with_count(view)
    }

    pub fn update_saved_view(
        &mut self,
        saved_view_id: i64,
        name: &str,
        query: &SavedViewQuery,
        pinned: bool,
    ) -> ApplicationResult<SavedViewWithCount> {
        let view = self
            .repository
            .update_saved_view(saved_view_id, name, query, pinned)?;
        self.saved_view_with_count(view)
    }

    pub fn delete_saved_view(&mut self, saved_view_id: i64) -> ApplicationResult<()> {
        Ok(self.repository.delete_saved_view(saved_view_id)?)
    }

    pub fn shelf_configuration(&self) -> ApplicationResult<Vec<ShelfConfigurationItem>> {
        Ok(self.repository.shelf_configuration()?)
    }

    pub fn replace_shelf_configuration(
        &mut self,
        items: &[ShelfConfigurationItem],
    ) -> ApplicationResult<Vec<ShelfConfigurationItem>> {
        Ok(self.repository.replace_shelf_configuration(items)?)
    }

    pub fn reset_shelf_configuration(&mut self) -> ApplicationResult<Vec<ShelfConfigurationItem>> {
        Ok(self.repository.reset_shelf_configuration()?)
    }

    pub fn collection(&self, collection_id: i64) -> ApplicationResult<CollectionSnapshot> {
        Ok(self.repository.collection(collection_id)?)
    }

    pub fn locate_collection(
        &self,
        collection_id: i64,
        query: &CollectionQuery,
    ) -> ApplicationResult<CollectionQueryLocation> {
        Ok(self.repository.locate_collection(collection_id, query)?)
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

    pub fn batch_add_collection_tag(
        &mut self,
        collection_ids: &[i64],
        tag_name: &str,
    ) -> ApplicationBatchReport {
        let tag_name = tag_name.trim();
        let items = collection_ids
            .iter()
            .map(|&collection_id| {
                let outcome = match self.repository.collection(collection_id) {
                    Ok(collection) if collection.tags.iter().any(|tag| tag == tag_name) => {
                        ApplicationBatchOutcome::Unchanged(collection)
                    }
                    Ok(_) => match self.add_collection_tag(collection_id, tag_name) {
                        Ok(collection) => ApplicationBatchOutcome::Succeeded(collection),
                        Err(error) => ApplicationBatchOutcome::Failed(error),
                    },
                    Err(error) => ApplicationBatchOutcome::Failed(error.into()),
                };
                ApplicationBatchItem {
                    collection_id,
                    outcome,
                }
            })
            .collect();
        ApplicationBatchReport { items }
    }

    pub fn batch_set_manual_metadata(
        &mut self,
        collection_ids: &[i64],
        field: MetadataField,
        value: MetadataValue,
    ) -> ApplicationBatchReport {
        let items = collection_ids
            .iter()
            .map(|&collection_id| {
                let outcome = match self.set_manual_metadata(collection_id, field, value.clone()) {
                    Ok(collection) => ApplicationBatchOutcome::Succeeded(collection),
                    Err(error) => ApplicationBatchOutcome::Failed(error),
                };
                ApplicationBatchItem {
                    collection_id,
                    outcome,
                }
            })
            .collect();
        ApplicationBatchReport { items }
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

    pub fn update_library_root(
        &mut self,
        root_id: i64,
        path: &Path,
        source: SourceKind,
        label: &str,
    ) -> ApplicationResult<LibraryRootSnapshot> {
        Ok(self
            .repository
            .update_library_root(root_id, path, source, label)?)
    }

    pub fn reactivate_library_root(
        &mut self,
        root_id: i64,
    ) -> ApplicationResult<LibraryRootSnapshot> {
        Ok(self.repository.reactivate_library_root(root_id)?)
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
        let stored = self.repository.stored_application_settings()?;
        let saved_reader_path = stored
            .as_ref()
            .map(|settings| settings.reader_path.clone())
            .unwrap_or_else(|| self.reader_path.clone());
        let saved_thumbnail_width = stored
            .as_ref()
            .map(|settings| settings.thumbnail_width)
            .unwrap_or(config.width);
        let saved_thumbnail_height = stored
            .as_ref()
            .map(|settings| settings.thumbnail_height)
            .unwrap_or(config.height);
        let saved_thumbnail_quality = stored
            .as_ref()
            .map(|settings| settings.thumbnail_quality)
            .unwrap_or(config.quality);
        let default_archive_root_id = stored
            .as_ref()
            .and_then(|settings| settings.default_archive_root_id);
        let library_batch_size = stored
            .as_ref()
            .map(|settings| settings.library_batch_size)
            .unwrap_or(DEFAULT_LIBRARY_BATCH_SIZE);
        Ok(ApplicationSettingsSnapshot {
            reader_path: self.reader_path.clone(),
            thumbnail_width: config.width,
            thumbnail_height: config.height,
            thumbnail_quality: config.quality,
            saved_reader_path,
            saved_thumbnail_width,
            saved_thumbnail_height,
            saved_thumbnail_quality,
            reader_overridden_by_environment: self.settings_overrides.reader_path.is_some(),
            thumbnail_size_overridden_by_environment: self
                .settings_overrides
                .thumbnail_size
                .is_some(),
            thumbnail_quality_overridden_by_environment: self
                .settings_overrides
                .thumbnail_quality
                .is_some(),
            default_archive_root_id,
            library_batch_size,
        })
    }

    pub fn save_application_settings(
        &mut self,
        reader_path: Option<PathBuf>,
        thumbnail_width: u32,
        thumbnail_height: u32,
        thumbnail_quality: u8,
        default_archive_root_id: Option<i64>,
        library_batch_size: u32,
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
        if let Some(root_id) = default_archive_root_id {
            let root = self
                .repository
                .library_root(root_id)
                .map_err(|error| match error {
                    StorageError::LibraryRootNotFound(id) => ApplicationError::InvalidSettings(
                        format!("找不到 library root {id}，無法設為預設典藏庫"),
                    ),
                    other => ApplicationError::from(other),
                })?;
            if root.source != SourceKind::Archive {
                return Err(ApplicationError::InvalidSettings(
                    "預設典藏庫必須是 archive 來源的 library root".to_owned(),
                ));
            }
            if !root.active {
                return Err(ApplicationError::InvalidSettings(
                    "預設典藏庫必須是啟用中的 library root".to_owned(),
                ));
            }
        }
        let saved = self.repository.save_application_settings(
            reader_path.as_deref(),
            thumbnail_width,
            thumbnail_height,
            thumbnail_quality,
            &effective_thumbnail.settings_fingerprint(),
            default_archive_root_id,
            library_batch_size,
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

    pub fn vocabulary_candidates(
        &self,
        field: Option<VocabularyField>,
    ) -> ApplicationResult<Vec<VocabularyCandidateGroup>> {
        Ok(self.repository.vocabulary_candidates(field)?)
    }

    pub fn vocabulary_suggestions(
        &self,
        field: VocabularyField,
        search: &str,
        limit: u32,
    ) -> ApplicationResult<Vec<VocabularySuggestion>> {
        Ok(self
            .repository
            .vocabulary_suggestions(field, search, limit)?)
    }

    pub fn vocabulary_merge_preflight(
        &self,
        field: VocabularyField,
        canonical: &str,
        variants: &[String],
    ) -> ApplicationResult<VocabularyMergePreflight> {
        Ok(self
            .repository
            .vocabulary_merge_preflight(field, canonical, variants)?)
    }

    pub fn merge_vocabulary(
        &mut self,
        field: VocabularyField,
        canonical: &str,
        variants: &[String],
    ) -> ApplicationResult<VocabularyMergeResult> {
        Ok(self
            .repository
            .merge_vocabulary(field, canonical, variants)?)
    }

    pub fn reject_vocabulary_group(
        &mut self,
        field: VocabularyField,
        values: &[String],
        reason: &str,
        removed: bool,
    ) -> ApplicationResult<usize> {
        Ok(self
            .repository
            .reject_vocabulary_group(field, values, reason, removed)?)
    }

    pub fn cover_candidates(
        &mut self,
        collection_id: i64,
        limit: usize,
    ) -> ApplicationResult<ApplicationCoverCandidates> {
        self.repository.collection(collection_id)?;
        let source_path = self.repository.active_collection_file_path(collection_id)?;
        let current_source = source_fingerprint(&source_path)?;
        let mut selection = self.repository.cover_selection(collection_id)?;
        if let Some(saved) = selection.as_ref() {
            let validation = validate_cover_candidate(&source_path, &saved.entry_path);
            let (status, error) = match validation {
                Ok(_) if saved.source_fingerprint == current_source => {
                    (CoverSelectionStatus::Valid, None)
                }
                Ok(_) => (
                    CoverSelectionStatus::SourceChanged,
                    Some("收藏來源自選擇封面後已變更；仍找到同名 entry，請確認封面".to_owned()),
                ),
                Err(error) => (CoverSelectionStatus::Missing, Some(error.message)),
            };
            if saved.validation_status != status || saved.validation_error != error {
                selection = self.repository.update_cover_selection_validation(
                    collection_id,
                    status,
                    error.as_deref(),
                )?;
            }
        }
        Ok(ApplicationCoverCandidates {
            source_fingerprint: current_source,
            selection: selection.map(Into::into),
            items: cover_candidates(&source_path, limit)?
                .into_iter()
                .map(Into::into)
                .collect(),
        })
    }

    pub fn cover_candidate_preview(
        &self,
        collection_id: i64,
        entry_path: &str,
    ) -> ApplicationResult<Vec<u8>> {
        self.repository.collection(collection_id)?;
        let source_path = self.repository.active_collection_file_path(collection_id)?;
        Ok(cover_candidate_preview(&source_path, entry_path, 240, 320)?)
    }

    pub fn select_cover(
        &mut self,
        collection_id: i64,
        entry_path: &str,
        expected_source_fingerprint: &str,
    ) -> ApplicationResult<ApplicationCoverSelection> {
        self.repository.collection(collection_id)?;
        let source_path = self.repository.active_collection_file_path(collection_id)?;
        let current_source = source_fingerprint(&source_path)?;
        if current_source != expected_source_fingerprint {
            return Err(ApplicationError::InvalidSettings(
                "收藏來源已在載入候選後變更，請重新整理候選封面".to_owned(),
            ));
        }
        let candidate = validate_cover_candidate(&source_path, entry_path)?;
        let selection = self.repository.save_cover_selection(
            collection_id,
            &candidate.entry_path,
            &current_source,
        )?;
        self.rebuild_thumbnail(collection_id)?;
        Ok(selection.into())
    }

    pub fn clear_cover_selection(
        &mut self,
        collection_id: i64,
    ) -> ApplicationResult<Option<ApplicationCoverSelection>> {
        self.repository.clear_cover_selection(collection_id)?;
        self.rebuild_thumbnail(collection_id)?;
        Ok(None)
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
        let selection = self.repository.cover_selection(collection_id)?;
        let source_fingerprint = cover_source_fingerprint(
            &source_path,
            selection
                .as_ref()
                .map(|selection| selection.entry_path.as_str()),
        )?;
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
            selected_entry: self
                .repository
                .cover_selection(collection_id)?
                .map(|selection| selection.entry_path),
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
                if error.kind == ThumbnailErrorKind::NoSupportedImage
                    && self.repository.cover_selection(collection_id)?.is_some()
                {
                    self.repository.update_cover_selection_validation(
                        collection_id,
                        CoverSelectionStatus::Missing,
                        Some(&error.message),
                    )?;
                }
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
        let selection = self.repository.cover_selection(collection_id)?;
        let source_fingerprint = cover_source_fingerprint(
            &source_path,
            selection
                .as_ref()
                .map(|selection| selection.entry_path.as_str()),
        )?;
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

    pub fn prepare_thumbnail_cache(
        &mut self,
        root_ids: &[i64],
    ) -> ApplicationResult<ThumbnailCachePreparation> {
        let preflight = self.thumbnail_cache_preflight(root_ids)?;
        let collection_ids = preflight.collection_ids;
        let mut prepared_collection_ids = Vec::with_capacity(collection_ids.len());
        let mut failed_collection_ids = Vec::new();
        for collection_id in collection_ids {
            match self.request_thumbnail_with_priority(collection_id, BATCH_THUMBNAIL_PRIORITY) {
                Ok(_) => prepared_collection_ids.push(collection_id),
                Err(_) => failed_collection_ids.push(collection_id),
            }
        }
        Ok(ThumbnailCachePreparation {
            collection_ids: prepared_collection_ids,
            failed_collection_ids,
        })
    }

    pub fn thumbnail_cache_preflight(
        &self,
        root_ids: &[i64],
    ) -> ApplicationResult<ThumbnailCachePreflight> {
        let config = self.thumbnail_config()?;
        let mut selected_root_ids = root_ids.to_vec();
        selected_root_ids.sort_unstable();
        selected_root_ids.dedup();
        if selected_root_ids.is_empty() {
            return Err(ApplicationError::InvalidSettings(
                "至少必須選擇一個資料夾來源".to_owned(),
            ));
        }
        let active_root_ids = self
            .repository
            .library_roots()?
            .into_iter()
            .filter(|root| root.active)
            .map(|root| root.id)
            .collect::<std::collections::HashSet<_>>();
        if selected_root_ids
            .iter()
            .any(|root_id| !active_root_ids.contains(root_id))
        {
            return Err(ApplicationError::InvalidSettings(
                "快取建立範圍包含不存在或已停用的資料夾來源".to_owned(),
            ));
        }
        let collection_ids = self
            .repository
            .active_collection_ids_for_roots(&selected_root_ids)?;
        let settings_fingerprint = config.settings_fingerprint();
        let ready = collection_ids
            .iter()
            .filter(|collection_id| {
                let Ok(state) = self.repository.thumbnail_state(**collection_id) else {
                    return false;
                };
                let Ok(source_path) = self.repository.active_collection_file_path(**collection_id)
                else {
                    return false;
                };
                let Ok(selection) = self.repository.cover_selection(**collection_id) else {
                    return false;
                };
                let Ok(current_source_fingerprint) = cover_source_fingerprint(
                    &source_path,
                    selection
                        .as_ref()
                        .map(|selection| selection.entry_path.as_str()),
                ) else {
                    return false;
                };
                let expected_cache_path = config.cache_path(**collection_id);
                state.status == ThumbnailStatus::Ready
                    && state.source_fingerprint == current_source_fingerprint
                    && state.settings_fingerprint == settings_fingerprint
                    && state.cache_path == expected_cache_path
                    && expected_cache_path.is_file()
            })
            .count();
        Ok(ThumbnailCachePreflight {
            root_ids: selected_root_ids,
            collection_ids,
            ready,
        })
    }

    pub fn thumbnail_failed_collection_ids(
        &self,
        collection_ids: &[i64],
    ) -> ApplicationResult<Vec<i64>> {
        Ok(self
            .repository
            .thumbnail_failed_or_missing_collection_ids(collection_ids)?)
    }

    pub fn retry_thumbnails(
        &mut self,
        collection_ids: &[i64],
    ) -> ApplicationResult<ThumbnailCachePreparation> {
        let mut collection_ids = collection_ids.to_vec();
        collection_ids.sort_unstable();
        collection_ids.dedup();
        let mut prepared_collection_ids = Vec::with_capacity(collection_ids.len());
        let mut failed_collection_ids = Vec::new();
        for collection_id in collection_ids {
            match self.rebuild_thumbnail(collection_id).and_then(|_| {
                self.request_thumbnail_with_priority(collection_id, BATCH_THUMBNAIL_PRIORITY)
            }) {
                Ok(_) => prepared_collection_ids.push(collection_id),
                Err(_) => failed_collection_ids.push(collection_id),
            }
        }
        Ok(ThumbnailCachePreparation {
            collection_ids: prepared_collection_ids,
            failed_collection_ids,
        })
    }

    pub fn thumbnail_status_counts(
        &self,
        collection_ids: &[i64],
    ) -> ApplicationResult<ThumbnailStatusCounts> {
        Ok(self.repository.thumbnail_status_counts(collection_ids)?)
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

    fn saved_view_with_count(
        &self,
        view: SavedViewSnapshot,
    ) -> ApplicationResult<SavedViewWithCount> {
        let result_count = self
            .repository
            .collections(&view.query.collection_query())?
            .total;
        Ok(SavedViewWithCount { view, result_count })
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

fn same_filename(path: &Path, filename: &str) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(filename))
}

fn record_preflight_difference(
    differences: &mut Vec<String>,
    label: &str,
    expected: usize,
    actual: usize,
) {
    if expected != actual {
        differences.push(format!(
            "{label}在預覽後發生變化：預覽 {expected}，實際 {actual}"
        ));
    }
}
