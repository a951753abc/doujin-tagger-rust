use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use doujin_app::external_search::{
    ExternalMetadataCandidate, ExternalMetadataProvider, ExternalSearchProviderError,
    ExternalSearchProviderIssue, ExternalSearchProviderResponse, ExternalSearchRequest,
    ExternalTagCandidate,
};
use doujin_app::archive::ArchiveMovePreflightStatus;
use doujin_app::rename::{RenameExpectedItem, RenamePreflightStatus};
use doujin_app::{
    ApplicationBatchOutcome, ApplicationError, ApplicationScanIssueKind, ApplicationScanMode,
    ApplicationScanOptions, ApplicationScanStatus, ApplicationService,
    ApplicationSettingsOverrides,
};
use doujin_files::RecycleBin;
use doujin_parser::domain::Authors;
use doujin_scanner::{ScanRoot, SourceKind};
use doujin_storage::collections::{ReviewQueueKind, ReviewQueueQuery};
use doujin_storage::covers::CoverSelectionStatus;
use doujin_storage::external_search_batches::ExternalSearchBatchStrategy;
use doujin_storage::jobs::{ExternalSearchErrorKind, ExternalSearchJobStatus};
use doujin_storage::lifecycle::{CandidateDecision, CollectionStatus};
use doujin_storage::metadata::{
    ConfidenceEvidence, MetadataAssertionDecision, MetadataField, MetadataValue,
};
use doujin_storage::scan::ScanRunStatus;
use doujin_storage::thumbnails::{
    BACKGROUND_THUMBNAIL_PRIORITY, BATCH_THUMBNAIL_PRIORITY, DEFAULT_THUMBNAIL_PRIORITY,
    ThumbnailStatus,
};
use doujin_storage::{CatalogRepository, StorageError};
use doujin_thumbnails::{
    ThumbnailConfig, ThumbnailGenerationSuccess, calculate_source_content_fingerprint,
    generate_thumbnail,
};
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
use rusqlite::Connection;
use zip::write::SimpleFileOptions;

#[derive(Debug, Clone, Copy)]
struct NoopRecycleBin;

impl RecycleBin for NoopRecycleBin {
    fn recycle(&self, _path: &Path) -> Result<(), String> {
        Ok(())
    }
}

fn confidence(total: f64, exact_identifier: bool) -> ConfidenceEvidence {
    ConfidenceEvidence {
        total,
        source_reliability: 0.95,
        identifier_match: if exact_identifier { 1.0 } else { 0.5 },
        string_similarity: 0.9,
        rule_certainty: 0.9,
        reliable_identifier_exact_match: exact_identifier,
        reason: "application provider test".to_owned(),
    }
}

struct PartialProvider;

impl ExternalMetadataProvider for PartialProvider {
    fn search(
        &self,
        request: &ExternalSearchRequest,
    ) -> Result<ExternalSearchProviderResponse, ExternalSearchProviderError> {
        assert!(request.job_id > 0);
        assert_eq!(Some("filename title"), request.collection.title.as_deref());
        assert!(request.identifiers.is_empty());
        assert_eq!(
            vec![
                MetadataField::Title,
                MetadataField::Circle,
                MetadataField::Parody,
            ],
            request.fields
        );
        Ok(ExternalSearchProviderResponse {
            candidates: vec![
                ExternalMetadataCandidate {
                    field: MetadataField::Title,
                    value: MetadataValue::Text("official title".to_owned()),
                    source_reference: "mock:item-1:title".to_owned(),
                    confidence: confidence(0.96, true),
                },
                ExternalMetadataCandidate {
                    field: MetadataField::Circle,
                    value: MetadataValue::Text("Official Circle".to_owned()),
                    source_reference: "mock:item-1:circle".to_owned(),
                    confidence: confidence(0.8, false),
                },
            ],
            tags: vec![ExternalTagCandidate {
                name: "female:big breasts".to_owned(),
                source_reference: "mock:item-1:tag:female:big breasts".to_owned(),
                confidence: confidence(0.88, false),
            }],
            issues: vec![ExternalSearchProviderIssue {
                field: Some(MetadataField::Parody),
                kind: ExternalSearchErrorKind::ProviderUnavailable,
                message: "parody endpoint unavailable".to_owned(),
            }],
        })
    }
}

struct CandidateIssueProvider;

impl ExternalMetadataProvider for CandidateIssueProvider {
    fn search(
        &self,
        request: &ExternalSearchRequest,
    ) -> Result<ExternalSearchProviderResponse, ExternalSearchProviderError> {
        assert_eq!(vec![MetadataField::Authors], request.fields);
        Ok(ExternalSearchProviderResponse {
            candidates: vec![ExternalMetadataCandidate {
                field: MetadataField::Authors,
                value: MetadataValue::Authors(Authors {
                    raw: Some("Candidate Author".to_owned()),
                    values: vec!["Candidate Author".to_owned()],
                }),
                source_reference: "mock:candidate-lifecycle:authors".to_owned(),
                confidence: confidence(0.8, false),
            }],
            tags: Vec::new(),
            issues: vec![ExternalSearchProviderIssue {
                field: Some(MetadataField::Authors),
                kind: ExternalSearchErrorKind::ProviderUnavailable,
                message: "secondary authors source unavailable".to_owned(),
            }],
        })
    }
}

struct IdentifierProvider;

impl ExternalMetadataProvider for IdentifierProvider {
    fn search(
        &self,
        request: &ExternalSearchRequest,
    ) -> Result<ExternalSearchProviderResponse, ExternalSearchProviderError> {
        assert_eq!(1, request.identifiers.len());
        assert_eq!("RJ", request.identifiers[0].scheme);
        assert_eq!("RJ407766", request.identifiers[0].value);
        Ok(ExternalSearchProviderResponse {
            candidates: vec![ExternalMetadataCandidate {
                field: MetadataField::Title,
                value: MetadataValue::Text("identified title".to_owned()),
                source_reference: "mock:RJ407766".to_owned(),
                confidence: confidence(0.98, true),
            }],
            tags: Vec::new(),
            issues: Vec::new(),
        })
    }
}

struct FailingProvider {
    kind: ExternalSearchErrorKind,
}

impl ExternalMetadataProvider for FailingProvider {
    fn search(
        &self,
        _request: &ExternalSearchRequest,
    ) -> Result<ExternalSearchProviderResponse, ExternalSearchProviderError> {
        Err(ExternalSearchProviderError {
            kind: self.kind,
            message: "provider failure".to_owned(),
        })
    }
}

struct BatchProvider;

impl ExternalMetadataProvider for BatchProvider {
    fn search(
        &self,
        request: &ExternalSearchRequest,
    ) -> Result<ExternalSearchProviderResponse, ExternalSearchProviderError> {
        let title = request.collection.title.as_deref().unwrap_or_default();
        match title {
            "success" => Ok(ExternalSearchProviderResponse {
                candidates: vec![ExternalMetadataCandidate {
                    field: MetadataField::Title,
                    value: MetadataValue::Text("success enriched".to_owned()),
                    source_reference: "batch:success".to_owned(),
                    confidence: confidence(0.96, true),
                }],
                tags: Vec::new(),
                issues: Vec::new(),
            }),
            "partial" => Ok(ExternalSearchProviderResponse {
                candidates: vec![ExternalMetadataCandidate {
                    field: MetadataField::Title,
                    value: MetadataValue::Text("partial enriched".to_owned()),
                    source_reference: "batch:partial".to_owned(),
                    confidence: confidence(0.8, false),
                }],
                tags: Vec::new(),
                issues: vec![ExternalSearchProviderIssue {
                    field: Some(MetadataField::Circle),
                    kind: ExternalSearchErrorKind::InvalidResponse,
                    message: "circle payload invalid".to_owned(),
                }],
            }),
            "retry" => Err(ExternalSearchProviderError {
                kind: ExternalSearchErrorKind::Network,
                message: "network unavailable".to_owned(),
            }),
            "failed" => Err(ExternalSearchProviderError {
                kind: ExternalSearchErrorKind::Unsupported,
                message: "provider does not support this collection".to_owned(),
            }),
            _ => Err(ExternalSearchProviderError {
                kind: ExternalSearchErrorKind::InvalidResponse,
                message: format!("unexpected test title: {title}"),
            }),
        }
    }
}

struct BatchEvidenceProvider;

impl ExternalMetadataProvider for BatchEvidenceProvider {
    fn search(
        &self,
        request: &ExternalSearchRequest,
    ) -> Result<ExternalSearchProviderResponse, ExternalSearchProviderError> {
        let medium = request.collection.title.as_deref() == Some("medium");
        Ok(ExternalSearchProviderResponse {
            candidates: vec![ExternalMetadataCandidate {
                field: MetadataField::Title,
                value: MetadataValue::Text(if medium {
                    "medium candidate".to_owned()
                } else {
                    "search-only evidence".to_owned()
                }),
                source_reference: format!("batch:evidence:{}", request.collection.id),
                confidence: confidence(if medium { 0.8 } else { 0.5 }, false),
            }],
            tags: Vec::new(),
            issues: Vec::new(),
        })
    }
}

struct TestTree {
    path: PathBuf,
}

impl TestTree {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "doujin-app-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test tree");
        Self { path }
    }

    fn library(&self) -> PathBuf {
        self.path.join("library")
    }

    fn zip(&self, filename: &str) -> PathBuf {
        let path = self.library().join(filename);
        fs::create_dir_all(path.parent().expect("zip parent")).expect("create library");
        fs::write(&path, b"zip placeholder").expect("create zip");
        path
    }

    fn image_zip(&self, filename: &str, entries: &[(&str, [u8; 4])]) -> PathBuf {
        let path = self.library().join(filename);
        fs::create_dir_all(path.parent().expect("zip parent")).expect("create library");
        let mut archive = zip::ZipWriter::new(fs::File::create(&path).expect("create image ZIP"));
        for (entry, color) in entries {
            archive
                .start_file(*entry, SimpleFileOptions::default())
                .expect("start image entry");
            let image = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(24, 32, Rgba(*color)));
            let mut encoded = Cursor::new(Vec::new());
            image
                .write_to(&mut encoded, ImageFormat::Png)
                .expect("encode image entry");
            archive
                .write_all(&encoded.into_inner())
                .expect("write image entry");
        }
        archive.finish().expect("finish image ZIP");
        path
    }

    fn root(&self) -> ScanRoot {
        ScanRoot {
            path: self.library(),
            source: SourceKind::Downloads,
            label: "下載區".to_owned(),
        }
    }

    fn database(&self) -> PathBuf {
        self.path.join("catalog.db")
    }
}

impl Drop for TestTree {
    fn drop(&mut self) {
        if self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("doujin-app-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[test]
fn duplicate_workers_process_100_plus_isolate_failure_retry_and_reuse_cache() {
    let tree = TestTree::new("duplicate-worker-large");
    for index in 0..101 {
        tree.image_zip(
            &format!("[Circle {index:03}] work {index:03}.zip"),
            &[("001.png", [index as u8, 20, 30, 255])],
        );
    }
    let corrupt = tree.zip("[Circle bad] corrupt.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[tree.root()])
        .expect("scan 102 collections");

    let job = application
        .start_duplicate_scan()
        .expect("start duplicate scan");
    assert_eq!(102, job.total);
    assert_eq!(2, job.concurrency_limit);
    while let Some(request) = application
        .claim_duplicate_fingerprint()
        .expect("claim duplicate work")
    {
        let result = calculate_source_content_fingerprint(&request.source_path);
        application
            .finish_duplicate_fingerprint(&request, result)
            .expect("finish duplicate work");
    }
    let completed = application
        .duplicate_scan_job(job.id)
        .expect("completed duplicate scan");
    assert_eq!(101, completed.processed);
    assert_eq!(1, completed.failed);
    assert_eq!(0, completed.pending);

    let mut archive = zip::ZipWriter::new(fs::File::create(&corrupt).expect("replace corrupt ZIP"));
    archive
        .start_file("001.png", SimpleFileOptions::default())
        .expect("start replacement image");
    let image =
        DynamicImage::ImageRgba8(ImageBuffer::from_pixel(24, 32, Rgba([200, 100, 50, 255])));
    let mut encoded = Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, ImageFormat::Png)
        .expect("encode replacement image");
    archive
        .write_all(&encoded.into_inner())
        .expect("write replacement image");
    archive.finish().expect("finish replacement ZIP");

    application
        .retry_duplicate_scan_failures(job.id)
        .expect("retry failed item");
    let retry = application
        .claim_duplicate_fingerprint()
        .expect("claim retry")
        .expect("retry request");
    let result = calculate_source_content_fingerprint(&retry.source_path);
    application
        .finish_duplicate_fingerprint(&retry, result)
        .expect("finish retry");
    let completed = application
        .duplicate_scan_job(job.id)
        .expect("completed retried scan");
    assert_eq!(102, completed.processed);
    assert_eq!(0, completed.failed);

    let cached_job = application
        .start_duplicate_scan()
        .expect("start cached duplicate scan");
    assert!(
        application
            .claim_duplicate_fingerprint()
            .expect("cached scan finishes without hashing")
            .is_none()
    );
    let cached = application
        .duplicate_scan_job(cached_job.id)
        .expect("cached scan status");
    assert_eq!(102, cached.processed);
    assert_eq!(102, cached.reused_cache);
    assert_eq!(0, cached.failed);
}

#[test]
fn successful_scan_is_idempotent_and_persists_each_run() {
    let tree = TestTree::new("success");
    tree.zip("[circle] title.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);

    let first = application.run_scan(&[tree.root()]).expect("first scan");
    let second = application.run_scan(&[tree.root()]).expect("second scan");

    assert_eq!(ApplicationScanStatus::Succeeded, first.status);
    assert_eq!(1, first.summary.added);
    assert_eq!(ApplicationScanStatus::Succeeded, second.status);
    assert_eq!(0, second.summary.added);
    assert_eq!(1, second.summary.skipped);
    assert_eq!(
        1,
        application
            .repository()
            .collection_count()
            .expect("collections")
    );
    assert_eq!(
        1,
        application
            .repository()
            .parser_run_count()
            .expect("parser runs")
    );
    assert_eq!(
        2,
        application
            .repository()
            .scan_run_count()
            .expect("scan runs")
    );
}

#[test]
fn rename_preflight_handles_more_than_one_hundred_and_applies_without_confirmation_per_item() {
    let tree = TestTree::new("rename-large-batch");
    for index in 0..101 {
        tree.zip(&format!("title {index:03}.zip"));
    }
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&[tree.root()]).expect("scan batch");
    let collection_ids = (0..101)
        .map(|index| {
            application
                .repository()
                .collection_id_for_current_path(
                    &tree.library().join(format!("title {index:03}.zip")),
                )
                .expect("path lookup")
                .expect("collection id")
        })
        .collect::<Vec<_>>();

    let preflight = application
        .rename_preflight(&collection_ids, "renamed - {title}")
        .expect("rename preflight");
    assert_eq!(101, preflight.summary.total);
    assert_eq!(101, preflight.summary.safe);
    assert!(
        preflight
            .items
            .iter()
            .all(|item| item.status == RenamePreflightStatus::Safe)
    );
    let expected = preflight
        .items
        .into_iter()
        .map(|item| RenameExpectedItem {
            collection_id: item.collection_id,
            expected_source: item.expected_source,
            expected_destination: item.expected_destination.expect("destination"),
        })
        .collect::<Vec<_>>();
    let report = application
        .apply_rename_preflight("renamed - {title}", &expected)
        .expect("apply batch rename");

    assert_eq!(101, report.succeeded());
    assert_eq!(0, report.failed());
    assert!(tree.library().join("renamed - title 000.zip").exists());
    assert!(tree.library().join("renamed - title 100.zip").exists());
}

#[test]
fn rename_apply_rejects_stale_preflight_and_preflight_classifies_blockers() {
    let tree = TestTree::new("rename-stale");
    let first = tree.zip("first.zip");
    let second = tree.zip("second.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&[tree.root()]).expect("scan");
    let first_id = application
        .repository()
        .collection_id_for_current_path(&first)
        .expect("lookup")
        .expect("first id");
    let second_id = application
        .repository()
        .collection_id_for_current_path(&second)
        .expect("lookup")
        .expect("second id");

    let collision = application
        .rename_preflight(&[first_id, second_id], "same")
        .expect("collision preflight");
    assert_eq!(2, collision.summary.collision);
    let missing = application
        .rename_preflight(&[first_id], "[{event}]")
        .expect("missing preflight");
    assert_eq!(
        RenamePreflightStatus::MissingMetadata,
        missing.items[0].status
    );
    let illegal = application
        .rename_preflight(&[first_id], "bad:name")
        .expect("illegal preflight");
    assert_eq!(RenamePreflightStatus::Illegal, illegal.items[0].status);

    let preview = application
        .rename_preflight(&[first_id], "renamed - {title}")
        .expect("safe preflight");
    let item = &preview.items[0];
    let expected = RenameExpectedItem {
        collection_id: item.collection_id,
        expected_source: item.expected_source.clone(),
        expected_destination: item.expected_destination.clone().expect("destination"),
    };
    let external_path = tree.library().join("externally changed.zip");
    fs::rename(&first, &external_path).expect("external source change");
    let report = application
        .apply_rename_preflight("renamed - {title}", &[expected])
        .expect("stale apply report");
    assert_eq!(1, report.failed());
    assert_eq!(
        0,
        application
            .repository()
            .file_operation_count()
            .expect("journal")
    );

    fs::rename(&external_path, &first).expect("restore source for retry");
    let retry_preview = application
        .rename_preflight(&[first_id], "renamed - {title}")
        .expect("retry preflight");
    assert_eq!(1, retry_preview.summary.safe);
    let retry_item = &retry_preview.items[0];
    let retry = RenameExpectedItem {
        collection_id: first_id,
        expected_source: retry_item.expected_source.clone(),
        expected_destination: retry_item
            .expected_destination
            .clone()
            .expect("retry destination"),
    };
    assert_eq!(
        1,
        application
            .apply_rename_preflight("renamed - {title}", &[retry])
            .expect("retry apply")
            .succeeded()
    );
}

#[test]
fn rename_preflight_explicitly_reports_image_folder_as_unsupported() {
    let tree = TestTree::new("rename-folder-unsupported");
    let source = tree.zip("folder-lifecycle-placeholder.zip");
    let mut application = ApplicationService::new(
        CatalogRepository::open(tree.database()).expect("open catalog"),
        NoopRecycleBin,
    );
    application.run_scan(&[tree.root()]).expect("scan");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&source)
        .expect("lookup")
        .expect("id");
    drop(application);
    let connection = Connection::open(tree.database()).expect("open raw catalog");
    connection
        .execute(
            "UPDATE collections SET media_kind = 'image_folder' WHERE id = ?1",
            [collection_id],
        )
        .expect("mark image folder lifecycle type");
    drop(connection);
    let application = ApplicationService::new(
        CatalogRepository::open(tree.database()).expect("reopen catalog"),
        NoopRecycleBin,
    );

    let preflight = application
        .rename_preflight(&[collection_id], "{title}")
        .expect("folder preflight");
    assert_eq!(1, preflight.summary.unsupported);
    assert_eq!(
        RenamePreflightStatus::Unsupported,
        preflight.items[0].status
    );
    assert!(
        preflight.items[0]
            .message
            .as_deref()
            .is_some_and(|message| message.contains("第一版只支援 ZIP"))
    );
}

#[test]
fn scan_preflight_is_read_only_and_reports_rename_diff() {
    let tree = TestTree::new("preflight-read-only");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let renamed = tree.library().join("(C77) [circle] title.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let application = ApplicationService::new(repository, NoopRecycleBin);

    let preflight = application
        .preflight_scan(&[tree.root()])
        .expect("preflight scan");

    assert_eq!(1, preflight.expectation.new_collections);
    assert_eq!(1, preflight.expectation.planned_renames);
    assert_eq!(original, preflight.renames[0].before);
    assert_eq!(renamed, preflight.renames[0].after);
    assert!(original.exists());
    assert!(!renamed.exists());
    assert_eq!(
        0,
        application
            .repository()
            .collection_count()
            .expect("collection count")
    );
    assert_eq!(
        0,
        application
            .repository()
            .scan_run_count()
            .expect("scan run count")
    );
}

#[test]
fn no_rename_scan_indexes_only_the_real_existing_path() {
    let tree = TestTree::new("no-rename");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let renamed = tree.library().join("(C77) [circle] title.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    let preflight = application
        .preflight_scan(&[tree.root()])
        .expect("preflight scan");

    let report = application
        .run_scan_with_options(
            &[tree.root()],
            ApplicationScanOptions {
                mode: ApplicationScanMode::NoRename,
                expected: Some(preflight.expectation),
            },
        )
        .expect("no-rename scan");

    assert_eq!(1, report.summary.added);
    assert_eq!(0, report.summary.renamed);
    assert!(report.summary.preflight_differences.is_empty());
    assert!(original.exists());
    assert!(!renamed.exists());
    assert!(
        application
            .repository()
            .collection_id_for_current_path(&original)
            .expect("path lookup")
            .is_some()
    );
    assert_eq!(
        None,
        application
            .repository()
            .collection_id_for_current_path(&renamed)
            .expect("renamed path lookup")
    );
}

#[test]
fn apply_revalidates_collision_and_reports_preflight_drift() {
    let tree = TestTree::new("preflight-toctou");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let renamed = tree.library().join("(C77) [circle] title.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    let preflight = application
        .preflight_scan(&[tree.root()])
        .expect("preflight scan");
    fs::write(&renamed, b"appeared after preflight").expect("create collision");

    let report = application
        .run_scan_with_options(
            &[tree.root()],
            ApplicationScanOptions {
                mode: ApplicationScanMode::ApplySafeRenames,
                expected: Some(preflight.expectation),
            },
        )
        .expect("apply scan");

    assert!(original.exists());
    assert_eq!(0, report.summary.renamed);
    assert_eq!(1, report.summary.normalization_warnings);
    assert!(
        report
            .summary
            .preflight_differences
            .iter()
            .any(|difference| difference.contains("實體改名"))
    );
}

#[test]
fn batch_tag_reports_success_unchanged_and_failure_per_collection() {
    let tree = TestTree::new("batch-tag");
    tree.zip("[circle] first.zip");
    tree.zip("[circle] second.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[tree.root()])
        .expect("scan collections");
    let collection_ids = application
        .collections(&Default::default())
        .expect("list collections")
        .items
        .into_iter()
        .map(|collection| collection.id)
        .collect::<Vec<_>>();

    let first = application
        .batch_add_collection_tag(&[collection_ids[0], collection_ids[1], 999_999], "favorite");
    assert!(matches!(
        &first.items[0].outcome,
        ApplicationBatchOutcome::Succeeded(_)
    ));
    assert!(matches!(
        &first.items[1].outcome,
        ApplicationBatchOutcome::Succeeded(_)
    ));
    assert!(matches!(
        &first.items[2].outcome,
        ApplicationBatchOutcome::Failed(ApplicationError::Storage(
            StorageError::CollectionNotFound(999_999)
        ))
    ));

    let second = application.batch_add_collection_tag(&collection_ids, "favorite");
    assert!(
        second
            .items
            .iter()
            .all(|item| matches!(&item.outcome, ApplicationBatchOutcome::Unchanged(_)))
    );
}

#[test]
fn complete_scan_tombstones_missing_collection_and_links_all_same_filename_candidates() {
    let tree = TestTree::new("missing-reconciliation");
    let filename = "[circle] same-name.zip";
    let old_path = tree.zip(filename);
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&[tree.root()]).expect("initial scan");
    let old_id = application
        .repository()
        .collection_id_for_current_path(&old_path)
        .expect("old path lookup")
        .expect("old collection");
    application
        .add_collection_tag(old_id, "legacy-tag")
        .expect("tag old collection");

    fs::remove_file(&old_path).expect("remove old path");
    let first_path = tree.library().join("candidate-a").join(filename);
    let second_path = tree.library().join("candidate-b").join(filename);
    for path in [&first_path, &second_path] {
        fs::create_dir_all(path.parent().expect("candidate parent"))
            .expect("create candidate directory");
        fs::write(path, b"candidate zip").expect("create candidate");
    }

    let report = application
        .run_scan(&[tree.root()])
        .expect("reconciliation scan");

    assert_eq!(ApplicationScanStatus::Succeeded, report.status);
    assert_eq!(2, report.summary.added);
    assert_eq!(1, report.summary.tombstoned);
    assert_eq!(2, report.summary.candidate_links_created);
    assert_eq!(
        CollectionStatus::Tombstone,
        application
            .repository()
            .collection_status(old_id)
            .expect("old status")
    );
    let candidates = application.tombstone_candidates().expect("candidate links");
    assert_eq!(2, candidates.len());
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.decision == CandidateDecision::Pending)
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.tombstone_path == old_path)
    );

    let first_id = application
        .repository()
        .collection_id_for_current_path(&first_path)
        .expect("first lookup")
        .expect("first candidate");
    let second_id = application
        .repository()
        .collection_id_for_current_path(&second_path)
        .expect("second lookup")
        .expect("second candidate");
    assert_ne!(old_id, first_id);
    assert_ne!(old_id, second_id);
    assert!(
        application
            .collection(first_id)
            .expect("first candidate detail")
            .tags
            .is_empty()
    );
    assert!(
        application
            .collection(second_id)
            .expect("second candidate detail")
            .tags
            .is_empty()
    );

    let decided = application
        .decide_tombstone_candidate(old_id, first_id, CandidateDecision::Rejected)
        .expect("reject candidate");
    assert_eq!(CandidateDecision::Rejected, decided.decision);
    assert!(decided.decided_at.is_some());
    assert_eq!(
        CollectionStatus::Tombstone,
        application
            .repository()
            .collection_status(old_id)
            .expect("decision does not merge identity")
    );
}

#[test]
fn unavailable_root_never_tombstones_its_collections() {
    let tree = TestTree::new("unavailable-root");
    let path = tree.zip("[circle] preserved.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&[tree.root()]).expect("initial scan");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&path)
        .expect("path lookup")
        .expect("collection");
    fs::remove_dir_all(tree.library()).expect("disconnect root fixture");

    let report = application
        .run_scan(&[tree.root()])
        .expect("missing-root scan");

    assert_eq!(ApplicationScanStatus::Partial, report.status);
    assert_eq!(1, report.summary.missing_roots);
    assert_eq!(0, report.summary.tombstoned);
    assert_eq!(
        CollectionStatus::Active,
        application
            .repository()
            .collection_status(collection_id)
            .expect("collection remains active")
    );
}

#[test]
fn missing_collection_without_same_filename_candidate_is_left_for_separate_policy() {
    let tree = TestTree::new("missing-without-candidate");
    let path = tree.zip("[circle] no-candidate.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&[tree.root()]).expect("initial scan");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&path)
        .expect("path lookup")
        .expect("collection");
    fs::remove_file(&path).expect("remove collection file");

    let report = application
        .run_scan(&[tree.root()])
        .expect("candidate reconciliation scan");

    assert_eq!(ApplicationScanStatus::Succeeded, report.status);
    assert_eq!(0, report.summary.tombstoned);
    assert_eq!(0, report.summary.candidate_links_created);
    assert_eq!(
        CollectionStatus::Active,
        application
            .repository()
            .collection_status(collection_id)
            .expect("unrelated missing policy is not inferred")
    );
}

#[test]
fn ingest_failure_is_partial_and_does_not_rollback_successful_items() {
    let tree = TestTree::new("partial");
    tree.zip("[circle] good.zip");
    tree.zip("[circle] bad.zip");
    drop(CatalogRepository::open(tree.database()).expect("initialize catalog"));
    let connection = Connection::open(tree.database()).expect("open fixture database");
    connection
        .execute_batch(
            "CREATE TRIGGER reject_bad_location
             BEFORE INSERT ON collection_locations
             WHEN new.filename = '[circle] bad.zip'
             BEGIN
                 SELECT raise(ABORT, 'fixture rejects bad.zip');
             END;",
        )
        .expect("install failure trigger");
    drop(connection);
    let repository = CatalogRepository::open(tree.database()).expect("reopen catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);

    let report = application.run_scan(&[tree.root()]).expect("partial scan");

    assert_eq!(ApplicationScanStatus::Partial, report.status);
    assert_eq!(1, report.summary.added);
    assert_eq!(1, report.summary.ingest_failed);
    assert_eq!(1, report.issues.len());
    assert_eq!(ApplicationScanIssueKind::Ingest, report.issues[0].kind);
    assert_eq!(
        1,
        application
            .repository()
            .collection_count()
            .expect("one success")
    );
    let run = application
        .repository()
        .scan_run(report.scan_run_id)
        .expect("persisted run");
    assert_eq!(ScanRunStatus::Partial, run.status);
    let summary: serde_json::Value =
        serde_json::from_str(run.summary_json.as_deref().expect("persisted summary"))
            .expect("summary JSON");
    assert_eq!(1, summary["added"]);
    assert_eq!(1, summary["ingest_failed"]);
    let issues = application
        .repository()
        .scan_issues(report.scan_run_id)
        .expect("persisted issues");
    assert_eq!("ingest", issues[0].kind);
}

#[test]
fn no_roots_is_a_persisted_partial_result_without_catalog_changes() {
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);

    let report = application.run_scan(&[]).expect("no-root scan");

    assert_eq!(ApplicationScanStatus::Partial, report.status);
    assert_eq!(ApplicationScanIssueKind::NoRoots, report.issues[0].kind);
    assert_eq!(
        0,
        application
            .repository()
            .collection_count()
            .expect("no collections")
    );
    assert_eq!(
        ScanRunStatus::Partial,
        application
            .repository()
            .scan_run(report.scan_run_id)
            .expect("scan run")
            .status
    );
}

#[test]
fn second_scan_is_rejected_while_a_run_is_marked_running() {
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.begin_scan_run().expect("existing scan");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);

    let error = application
        .run_scan(&[])
        .expect_err("must reject second scan");

    assert!(matches!(
        error,
        ApplicationError::Storage(StorageError::ScanAlreadyRunning)
    ));
    assert_eq!(
        1,
        application.repository().scan_run_count().expect("one run")
    );
}

#[test]
fn external_search_provider_can_partially_succeed_without_rolling_back_candidates() {
    let tree = TestTree::new("external-search-partial");
    let path = tree.zip("[Circle] filename title.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[tree.root()])
        .expect("scan collection");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&path)
        .expect("collection lookup")
        .expect("collection ID");
    let job = application
        .enqueue_external_search(
            collection_id,
            &[
                MetadataField::Title,
                MetadataField::Circle,
                MetadataField::Parody,
            ],
        )
        .expect("enqueue external search")
        .job;

    let completed = application
        .run_external_search_job(job.id, &PartialProvider)
        .expect("run partial provider");

    assert_eq!(ExternalSearchJobStatus::Partial, completed.status);
    assert_eq!(1, completed.attempts);
    assert_eq!(None, completed.next_retry_at);
    let summary: serde_json::Value = serde_json::from_str(
        completed
            .result_json
            .as_deref()
            .expect("external search summary"),
    )
    .expect("decode summary");
    assert_eq!(2, summary["candidates_received"]);
    assert_eq!(1, summary["tags_received"]);
    assert_eq!(1, summary["tags_applied"]);
    assert_eq!(1, summary["auto_applied"]);
    assert_eq!(1, summary["suggestions"]);
    assert_eq!(0, summary["search_only"]);
    assert_eq!("parody", summary["issues"][0]["field"]);
    assert_eq!("provider_unavailable", summary["issues"][0]["kind"]);
    assert_eq!(
        "official title",
        application
            .collection(collection_id)
            .expect("updated collection")
            .title
            .as_deref()
            .expect("effective title")
    );
    assert_eq!(
        2,
        application
            .repository()
            .external_search_result_count()
            .expect("stored field results")
    );
    assert!(
        application
            .collection(collection_id)
            .expect("tagged collection")
            .tags
            .contains(&"female:big breasts".to_owned())
    );
}

#[test]
fn external_search_activity_tracks_exact_candidate_until_manual_decision() {
    let tree = TestTree::new("external-search-candidate-lifecycle");
    let path = tree.zip("[Circle] candidate lifecycle.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[tree.root()])
        .expect("scan candidate lifecycle collection");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&path)
        .expect("candidate lifecycle lookup")
        .expect("candidate lifecycle collection ID");
    let job = application
        .enqueue_external_search(collection_id, &[MetadataField::Authors])
        .expect("enqueue candidate lifecycle job")
        .job;
    let partial = application
        .run_external_search_job(job.id, &CandidateIssueProvider)
        .expect("run candidate lifecycle provider");
    assert_eq!(ExternalSearchJobStatus::Partial, partial.status);
    let summary: serde_json::Value = serde_json::from_str(
        partial
            .result_json
            .as_deref()
            .expect("candidate lifecycle summary"),
    )
    .expect("decode candidate lifecycle summary");
    let assertion_id = summary["suggestion_assertion_ids"][0]
        .as_i64()
        .expect("exact suggestion assertion ID");

    let unresolved = application
        .external_search_activity()
        .expect("unresolved candidate activity");
    assert_eq!(1, unresolved.actionable_count);
    assert!(
        unresolved
            .items
            .iter()
            .find(|item| item.job.id == job.id)
            .expect("candidate activity item")
            .actionable
    );
    assert_eq!(
        1,
        application
            .review_queue(&ReviewQueueQuery {
                kind: ReviewQueueKind::Candidate,
                ..ReviewQueueQuery::default()
            })
            .expect("candidate review queue before decision")
            .total
    );

    application
        .decide_metadata_assertion(
            collection_id,
            MetadataField::Authors,
            assertion_id,
            MetadataAssertionDecision::Select,
        )
        .expect("accept exact suggestion assertion");
    let resolved = application
        .external_search_activity()
        .expect("resolved candidate activity");
    assert_eq!(0, resolved.actionable_count);
    let resolved_item = resolved
        .items
        .iter()
        .find(|item| item.job.id == job.id)
        .expect("resolved candidate activity item");
    assert!(!resolved_item.actionable);
    assert_eq!(
        Some(doujin_storage::jobs::ExternalSearchActivityResolution::MetadataResolved),
        resolved_item.resolution
    );
    assert_eq!(
        0,
        application
            .review_queue(&ReviewQueueQuery {
                kind: ReviewQueueKind::Candidate,
                ..ReviewQueueQuery::default()
            })
            .expect("candidate review queue after decision")
            .total
    );
    assert_eq!(
        ExternalSearchJobStatus::Partial,
        application
            .external_search_job(job.id)
            .expect("preserved candidate job history")
            .status
    );
}

#[test]
fn external_search_request_carries_latest_parser_identifiers() {
    let tree = TestTree::new("external-search-identifiers");
    let path = tree.zip("[RJ407766] [Circle] filename title.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[tree.root()])
        .expect("scan collection");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&path)
        .expect("collection lookup")
        .expect("collection ID");
    let job = application
        .enqueue_external_search(collection_id, &[MetadataField::Title])
        .expect("enqueue external search")
        .job;

    let completed = application
        .run_external_search_job(job.id, &IdentifierProvider)
        .expect("run identifier provider");

    assert_eq!(ExternalSearchJobStatus::Succeeded, completed.status);
}

#[test]
fn external_search_batch_reuses_candidate_and_search_only_review_rules() {
    let tree = TestTree::new("external-search-batch-evidence");
    let medium_path = tree.zip("[Circle] medium.zip");
    let low_path = tree.zip("[Circle] low.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[tree.root()])
        .expect("scan collections");
    let collection_ids = [&medium_path, &low_path]
        .into_iter()
        .map(|path| {
            application
                .repository()
                .collection_id_for_current_path(path)
                .expect("collection lookup")
                .expect("collection ID")
        })
        .collect::<Vec<_>>();

    let batch = application
        .create_external_search_batch(
            &collection_ids,
            &[MetadataField::Title],
            ExternalSearchBatchStrategy::Specified,
        )
        .expect("create batch");
    assert_eq!(2, batch.summary.pending);
    for item in batch.items {
        application
            .run_external_search_job(item.job_id.expect("batch job"), &BatchEvidenceProvider)
            .expect("run existing external pipeline");
    }
    let completed = application
        .external_search_batch(batch.id)
        .expect("completed batch");
    assert_eq!(2, completed.summary.succeeded);
    let low_job = completed
        .items
        .iter()
        .find(|item| item.collection_id == collection_ids[1])
        .and_then(|item| item.job_id)
        .expect("low confidence job");
    let summary: serde_json::Value = serde_json::from_str(
        &application
            .external_search_job(low_job)
            .expect("read low confidence job")
            .result_json
            .expect("job summary"),
    )
    .expect("decode job summary");
    assert_eq!(1, summary["search_only"]);
    assert_eq!(0, summary["suggestions"]);

    let queue = application
        .review_queue(&ReviewQueueQuery {
            kind: ReviewQueueKind::Candidate,
            ..ReviewQueueQuery::default()
        })
        .expect("candidate review queue");
    assert_eq!(1, queue.total);
    assert_eq!(collection_ids[0], queue.items[0].collection.id);
}

#[test]
fn external_search_failures_retry_only_for_transient_error_kinds() {
    let tree = TestTree::new("external-search-failures");
    let transient_path = tree.zip("[Circle] transient.zip");
    let permanent_path = tree.zip("[Circle] permanent.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[tree.root()])
        .expect("scan collections");
    let transient_id = application
        .repository()
        .collection_id_for_current_path(&transient_path)
        .expect("transient collection lookup")
        .expect("transient collection ID");
    let permanent_id = application
        .repository()
        .collection_id_for_current_path(&permanent_path)
        .expect("permanent collection lookup")
        .expect("permanent collection ID");

    let transient_job = application
        .enqueue_external_search(transient_id, &[MetadataField::Title])
        .expect("enqueue transient job")
        .job;
    let transient_job = application
        .run_external_search_job(
            transient_job.id,
            &FailingProvider {
                kind: ExternalSearchErrorKind::Network,
            },
        )
        .expect("persist transient failure");
    assert_eq!(ExternalSearchJobStatus::Pending, transient_job.status);
    assert_eq!(
        Some(ExternalSearchErrorKind::Network),
        transient_job.error_kind
    );
    assert!(transient_job.next_retry_at.is_some());
    assert!(matches!(
        application
            .run_external_search_job(
                transient_job.id,
                &FailingProvider {
                    kind: ExternalSearchErrorKind::Network,
                },
            )
            .expect_err("retry cannot run before due time"),
        ApplicationError::Storage(StorageError::ExternalSearchJobUnavailable(id))
            if id == transient_job.id
    ));

    let permanent_job = application
        .enqueue_external_search(permanent_id, &[MetadataField::Title])
        .expect("enqueue permanent job")
        .job;
    let permanent_job = application
        .run_external_search_job(
            permanent_job.id,
            &FailingProvider {
                kind: ExternalSearchErrorKind::Unsupported,
            },
        )
        .expect("persist permanent failure");
    assert_eq!(ExternalSearchJobStatus::Failed, permanent_job.status);
    assert_eq!(
        Some(ExternalSearchErrorKind::Unsupported),
        permanent_job.error_kind
    );
    assert_eq!(None, permanent_job.next_retry_at);
}

#[test]
fn external_search_worker_drains_due_jobs_and_isolates_domain_outcomes() {
    let tree = TestTree::new("external-search-worker");
    let paths = [
        tree.zip("[Circle] success.zip"),
        tree.zip("[Circle] partial.zip"),
        tree.zip("[Circle] retry.zip"),
        tree.zip("[Circle] failed.zip"),
    ];
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[tree.root()])
        .expect("scan collections");
    let collection_ids = paths
        .iter()
        .map(|path| {
            application
                .repository()
                .collection_id_for_current_path(path)
                .expect("collection lookup")
                .expect("collection ID")
        })
        .collect::<Vec<_>>();
    let jobs = collection_ids
        .iter()
        .map(|collection_id| {
            application
                .enqueue_external_search(
                    *collection_id,
                    &[MetadataField::Title, MetadataField::Circle],
                )
                .expect("enqueue worker job")
                .job
        })
        .collect::<Vec<_>>();

    let report = application
        .run_due_external_search_jobs(&BatchProvider, 10)
        .expect("drain due jobs");

    assert_eq!(4, report.due);
    assert_eq!(4, report.processed);
    assert_eq!(1, report.succeeded);
    assert_eq!(1, report.partial);
    assert_eq!(1, report.retry_scheduled);
    assert_eq!(1, report.failed);
    assert!(report.issues.is_empty());
    let expected_statuses = [
        ExternalSearchJobStatus::Succeeded,
        ExternalSearchJobStatus::Partial,
        ExternalSearchJobStatus::Pending,
        ExternalSearchJobStatus::Failed,
    ];
    for (job, expected_status) in jobs.iter().zip(expected_statuses) {
        assert_eq!(
            expected_status,
            application
                .external_search_job(job.id)
                .expect("persisted worker job")
                .status
        );
    }
    let empty = application
        .run_due_external_search_jobs(&BatchProvider, 10)
        .expect("future retry is not due");
    assert_eq!(0, empty.due);
    assert_eq!(0, empty.processed);
}

#[test]
fn interrupted_external_search_is_recovered_once_on_startup() {
    let tree = TestTree::new("external-search-recovery");
    let path = tree.zip("[Circle] recovery.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[tree.root()])
        .expect("scan collection");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&path)
        .expect("collection lookup")
        .expect("collection ID");
    let job = application
        .enqueue_external_search(collection_id, &[MetadataField::Title])
        .expect("enqueue job")
        .job;
    let mut repository = application.into_repository();
    repository
        .start_external_search_job(job.id)
        .expect("simulate interrupted running job");
    let mut restarted = ApplicationService::new(repository, NoopRecycleBin);

    assert_eq!(
        1,
        restarted
            .recover_interrupted_external_search_jobs()
            .expect("recover interrupted job")
    );
    assert_eq!(
        0,
        restarted
            .recover_interrupted_external_search_jobs()
            .expect("second recovery is idempotent")
    );
    let recovered = restarted
        .external_search_job(job.id)
        .expect("recovered job");
    assert_eq!(ExternalSearchJobStatus::Pending, recovered.status);
    assert_eq!(
        Some(ExternalSearchErrorKind::WorkerInterrupted),
        recovered.error_kind
    );
    assert_eq!(1, recovered.attempts);
    assert_eq!(None, recovered.next_retry_at);
}

#[test]
fn environment_overrides_remain_effective_while_user_settings_are_persisted() {
    let tree = TestTree::new("settings-overrides");
    let environment_reader = tree.path.join("environment-reader.exe");
    let stored_reader = tree.path.join("stored-reader.exe");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let thumbnail_config =
        ThumbnailConfig::new(tree.path.join("cache"), 500, 600, 90).expect("thumbnail config");
    let mut application = ApplicationService::with_system_services_and_overrides(
        repository,
        Some(environment_reader.clone()),
        thumbnail_config,
        ApplicationSettingsOverrides {
            reader_path: Some(environment_reader.clone()),
            thumbnail_size: Some((500, 600)),
            thumbnail_quality: Some(90),
        },
    );

    let saved = application
        .save_application_settings(Some(stored_reader.clone()), 360, 480, 85, None)
        .expect("save overridden settings");

    assert_eq!(Some(environment_reader), saved.settings.reader_path);
    assert_eq!(500, saved.settings.thumbnail_width);
    assert_eq!(600, saved.settings.thumbnail_height);
    assert_eq!(90, saved.settings.thumbnail_quality);
    assert_eq!(
        Some(stored_reader.clone()),
        saved.settings.saved_reader_path
    );
    assert_eq!(360, saved.settings.saved_thumbnail_width);
    assert_eq!(480, saved.settings.saved_thumbnail_height);
    assert_eq!(85, saved.settings.saved_thumbnail_quality);
    assert!(saved.settings.reader_overridden_by_environment);
    assert!(saved.settings.thumbnail_size_overridden_by_environment);
    assert!(saved.settings.thumbnail_quality_overridden_by_environment);
    let stored = application
        .repository()
        .stored_application_settings()
        .expect("stored settings")
        .expect("settings row");
    assert_eq!(Some(stored_reader), stored.reader_path);
    assert_eq!(360, stored.thumbnail_width);
    assert_eq!(480, stored.thumbnail_height);
    assert_eq!(85, stored.thumbnail_quality);
}

#[test]
fn stale_thumbnail_result_is_ignored_after_settings_requeue() {
    let tree = TestTree::new("stale-thumbnail-result");
    let source = tree.zip("[circle] settings-race.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let thumbnail_config =
        ThumbnailConfig::new(tree.path.join("cache"), 300, 400, 80).expect("thumbnail config");
    let mut application =
        ApplicationService::with_thumbnails(repository, NoopRecycleBin, thumbnail_config);
    application.run_scan(&[tree.root()]).expect("scan");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&source)
        .expect("collection lookup")
        .expect("collection ID");
    application
        .request_thumbnail(collection_id)
        .expect("request thumbnail");
    application
        .start_thumbnail_generation(collection_id)
        .expect("start thumbnail");

    let saved = application
        .save_application_settings(None, 480, 640, 100, None)
        .expect("save settings");
    assert_eq!(1, saved.thumbnails_requeued);
    let state = application
        .finish_thumbnail_generation(
            collection_id,
            Ok(ThumbnailGenerationSuccess {
                width: 300,
                height: 400,
            }),
        )
        .expect("ignore stale result");

    assert_eq!(ThumbnailStatus::Pending, state.status);
    assert_eq!("480x640-q100-webp-v1", state.settings_fingerprint);
    assert_eq!(None, state.generated_width);
    assert_eq!(None, state.generated_height);
}

#[test]
fn manual_cover_persists_invalidates_cache_survives_settings_and_reports_missing_until_clear() {
    let tree = TestTree::new("manual-cover-lifecycle");
    let source = tree.image_zip(
        "[circle] manual cover.zip",
        &[
            ("page1.png", [255, 0, 0, 255]),
            ("pages/page2.png", [0, 0, 255, 255]),
        ],
    );
    let database = tree.database();
    let cache_dir = tree.path.join("cache");
    let repository = CatalogRepository::open(&database).expect("open catalog");
    let config = ThumbnailConfig::new(cache_dir.clone(), 300, 400, 80).expect("config");
    let mut application =
        ApplicationService::with_thumbnails(repository, NoopRecycleBin, config.clone());
    application.run_scan(&[tree.root()]).expect("scan");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&source)
        .expect("collection lookup")
        .expect("collection ID");

    application
        .request_thumbnail(collection_id)
        .expect("request auto thumbnail");
    let auto_request = application
        .start_thumbnail_generation(collection_id)
        .expect("start auto thumbnail");
    assert_eq!(None, auto_request.selected_entry);
    let auto_result = generate_thumbnail(&auto_request);
    application
        .finish_thumbnail_generation(collection_id, auto_result)
        .expect("finish auto thumbnail");
    assert!(config.cache_path(collection_id).is_file());
    let auto_fingerprint = application
        .repository()
        .thumbnail_state(collection_id)
        .expect("auto state")
        .source_fingerprint;

    let candidates = application
        .cover_candidates(collection_id, 24)
        .expect("cover candidates");
    assert_eq!(2, candidates.items.len());
    application
        .select_cover(
            collection_id,
            "pages/page2.png",
            &candidates.source_fingerprint,
        )
        .expect("select manual cover");
    assert!(!config.cache_path(collection_id).exists());
    let selected_state = application
        .repository()
        .thumbnail_state(collection_id)
        .expect("selected state");
    assert_eq!(ThumbnailStatus::Pending, selected_state.status);
    assert_ne!(auto_fingerprint, selected_state.source_fingerprint);
    assert!(selected_state.source_fingerprint.contains("cover:manual"));

    let settings = application
        .save_application_settings(None, 480, 640, 92, None)
        .expect("change thumbnail settings");
    assert_eq!(1, settings.thumbnails_requeued);
    let selected_request = application
        .start_thumbnail_generation(collection_id)
        .expect("start selected thumbnail after settings");
    assert_eq!(
        Some("pages/page2.png"),
        selected_request.selected_entry.as_deref()
    );
    drop(application);

    let repository = CatalogRepository::open(&database).expect("reopen catalog");
    let restarted_config = ThumbnailConfig::new(cache_dir, 480, 640, 92).expect("restarted config");
    let mut restarted =
        ApplicationService::with_thumbnails(repository, NoopRecycleBin, restarted_config);
    let persisted = restarted
        .cover_candidates(collection_id, 24)
        .expect("persistent cover selection")
        .selection
        .expect("saved selection");
    assert_eq!("pages/page2.png", persisted.entry_path);
    assert_eq!("valid", persisted.status);

    tree.image_zip(
        "[circle] manual cover.zip",
        &[("page1.png", [255, 0, 0, 255])],
    );
    let changed = restarted
        .cover_candidates(collection_id, 24)
        .expect("changed source candidates")
        .selection
        .expect("invalid selection remains");
    assert_eq!("missing", changed.status);
    let stored = restarted
        .repository()
        .cover_selection(collection_id)
        .expect("read stored selection")
        .expect("selection retained");
    assert_eq!(CoverSelectionStatus::Missing, stored.validation_status);
    assert_eq!("pages/page2.png", stored.entry_path);

    restarted
        .clear_cover_selection(collection_id)
        .expect("restore automatic cover");
    assert!(
        restarted
            .repository()
            .cover_selection(collection_id)
            .expect("selection after clear")
            .is_none()
    );
    let auto_request = restarted
        .start_thumbnail_generation(collection_id)
        .expect("start restored automatic thumbnail");
    assert_eq!(None, auto_request.selected_entry);
}

#[test]
fn idle_prewarm_enqueues_one_background_thumbnail_at_a_time() {
    let tree = TestTree::new("idle-thumbnail-prewarm");
    tree.zip("[circle] first.zip");
    tree.zip("[circle] second.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let thumbnail_config =
        ThumbnailConfig::new(tree.path.join("cache"), 300, 400, 80).expect("thumbnail config");
    let mut application =
        ApplicationService::with_thumbnails(repository, NoopRecycleBin, thumbnail_config);
    application.run_scan(&[tree.root()]).expect("scan");

    let first = application
        .prewarm_next_thumbnail()
        .expect("prewarm first thumbnail")
        .expect("first prewarm outcome");
    assert!(first.enqueued);
    assert_eq!(BACKGROUND_THUMBNAIL_PRIORITY, first.state.priority);
    assert!(
        application
            .due_thumbnails_with_min_priority(10, 1)
            .expect("foreground due thumbnails")
            .is_empty()
    );
    assert_eq!(
        vec![first.state.collection_id],
        application
            .due_thumbnails(10)
            .expect("background due thumbnail")
            .into_iter()
            .map(|state| state.collection_id)
            .collect::<Vec<_>>()
    );

    let second = application
        .prewarm_next_thumbnail()
        .expect("prewarm second thumbnail")
        .expect("second prewarm outcome");
    assert_ne!(first.state.collection_id, second.state.collection_id);
    assert_eq!(BACKGROUND_THUMBNAIL_PRIORITY, second.state.priority);
    assert_eq!(
        None,
        application
            .prewarm_next_thumbnail()
            .expect("prewarm complete")
    );
}

#[test]
fn thumbnail_cache_batch_promotes_selected_work_ahead_of_default_queue() {
    let tree = TestTree::new("thumbnail-cache-priority");
    let selected_directory = tree.path.join("selected");
    let ordinary_directory = tree.path.join("ordinary");
    fs::create_dir_all(&selected_directory).expect("create selected root");
    fs::create_dir_all(&ordinary_directory).expect("create ordinary root");
    let selected_path = selected_directory.join("[circle] selected.zip");
    let ordinary_path = ordinary_directory.join("[circle] ordinary.zip");
    fs::write(&selected_path, b"zip placeholder").expect("create selected zip");
    fs::write(&ordinary_path, b"zip placeholder").expect("create ordinary zip");

    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let thumbnail_config =
        ThumbnailConfig::new(tree.path.join("cache"), 300, 400, 80).expect("thumbnail config");
    let mut application =
        ApplicationService::with_thumbnails(repository, NoopRecycleBin, thumbnail_config);
    application
        .run_scan(&[
            ScanRoot {
                path: selected_directory,
                source: SourceKind::Archive,
                label: "選取區".to_owned(),
            },
            ScanRoot {
                path: ordinary_directory,
                source: SourceKind::Archive,
                label: "一般區".to_owned(),
            },
        ])
        .expect("scan roots");
    let selected_id = application
        .repository()
        .collection_id_for_current_path(&selected_path)
        .expect("selected collection lookup")
        .expect("selected collection ID");
    let ordinary_id = application
        .repository()
        .collection_id_for_current_path(&ordinary_path)
        .expect("ordinary collection lookup")
        .expect("ordinary collection ID");
    let selected_root_id = application
        .library_roots()
        .expect("library roots")
        .into_iter()
        .find(|root| root.label == "選取區")
        .expect("selected root")
        .id;

    application
        .request_thumbnail(ordinary_id)
        .expect("enqueue ordinary thumbnail");
    application
        .request_thumbnail(selected_id)
        .expect("enqueue selected thumbnail at default priority");
    let prepared = application
        .prepare_thumbnail_cache(&[selected_root_id])
        .expect("prepare selected thumbnail cache");

    assert_eq!(vec![selected_id], prepared.collection_ids);
    assert!(BATCH_THUMBNAIL_PRIORITY > DEFAULT_THUMBNAIL_PRIORITY);
    assert_eq!(
        BATCH_THUMBNAIL_PRIORITY,
        application
            .repository()
            .thumbnail_state(selected_id)
            .expect("selected thumbnail state")
            .priority
    );
    assert_eq!(
        DEFAULT_THUMBNAIL_PRIORITY,
        application
            .repository()
            .thumbnail_state(ordinary_id)
            .expect("ordinary thumbnail state")
            .priority
    );
    assert_eq!(
        vec![selected_id, ordinary_id],
        application
            .due_thumbnails(2)
            .expect("due thumbnails")
            .into_iter()
            .map(|state| state.collection_id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn archive_move_preflight_reports_collision_when_destination_is_existing_directory() {
    let tree = TestTree::new("archive-preflight-directory-collision");
    let source_path = tree.zip("[circle] work.zip");
    let archive_path = tree.path.join("archive");
    fs::create_dir_all(&archive_path).expect("create archive root");

    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&[tree.root()]).expect("scan downloads root");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&source_path)
        .expect("collection lookup")
        .expect("collection id");
    let filename = application
        .repository()
        .collection(collection_id)
        .expect("collection snapshot")
        .filename;
    fs::create_dir_all(archive_path.join("未分類").join(&filename))
        .expect("create colliding destination directory");
    let archive_root = application
        .register_library_root(&archive_path, SourceKind::Archive, "歸檔區")
        .expect("register archive root");

    let preflight = application
        .move_preflight(&[collection_id], archive_root.id)
        .expect("move preflight");

    assert_eq!(1, preflight.items.len());
    assert_eq!(
        ArchiveMovePreflightStatus::Collision,
        preflight.items[0].status
    );
    assert_eq!(1, preflight.summary.collision);
    assert_eq!(0, preflight.summary.ready);
    assert_eq!(0, preflight.summary.ready_unclassified);
}

/// dangling symlink（連結目標不存在）作為 move 目的地時，preflight 仍必須回報 collision，
/// 因為 `Path::exists()` 對 dangling symlink 一律回 false，用它判 collision 會誤放行到 Ready。
/// symlink 建立在 Windows 需要 Developer Mode 或系統管理員權限；權限不足時略過本測試
/// （比照 doujin-storage/src/exports.rs 的
/// `export_root_rejects_directory_symlink_when_creation_is_permitted` 寫法）。
#[cfg(windows)]
#[test]
fn archive_move_preflight_reports_collision_for_dangling_destination_symlink() {
    use std::os::windows::fs::symlink_file;

    let tree = TestTree::new("archive-preflight-symlink-collision");
    let source_path = tree.zip("[circle] work.zip");
    let archive_path = tree.path.join("archive");
    let event_directory = archive_path.join("未分類");
    fs::create_dir_all(&event_directory).expect("create archive event directory");

    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&[tree.root()]).expect("scan downloads root");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&source_path)
        .expect("collection lookup")
        .expect("collection id");
    let filename = application
        .repository()
        .collection(collection_id)
        .expect("collection snapshot")
        .filename;
    let destination = event_directory.join(&filename);
    let dangling_target = archive_path.join("does-not-exist.zip");
    if symlink_file(&dangling_target, &destination).is_err() {
        return;
    }
    let archive_root = application
        .register_library_root(&archive_path, SourceKind::Archive, "歸檔區")
        .expect("register archive root");

    let preflight = application
        .move_preflight(&[collection_id], archive_root.id)
        .expect("move preflight");

    assert_eq!(1, preflight.items.len());
    assert_eq!(
        ArchiveMovePreflightStatus::Collision,
        preflight.items[0].status
    );
}
