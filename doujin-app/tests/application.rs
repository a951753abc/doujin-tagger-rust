use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use doujin_app::external_search::{
    ExternalMetadataCandidate, ExternalMetadataProvider, ExternalSearchProviderError,
    ExternalSearchProviderIssue, ExternalSearchProviderResponse, ExternalSearchRequest,
    ExternalTagCandidate,
};
use doujin_app::{
    ApplicationError, ApplicationScanIssueKind, ApplicationScanStatus, ApplicationService,
    ApplicationSettingsOverrides,
};
use doujin_files::RecycleBin;
use doujin_scanner::{ScanRoot, SourceKind};
use doujin_storage::jobs::{ExternalSearchErrorKind, ExternalSearchJobStatus};
use doujin_storage::lifecycle::{CandidateDecision, CollectionStatus};
use doujin_storage::metadata::{ConfidenceEvidence, MetadataField, MetadataValue};
use doujin_storage::scan::ScanRunStatus;
use doujin_storage::thumbnails::{BACKGROUND_THUMBNAIL_PRIORITY, ThumbnailStatus};
use doujin_storage::{CatalogRepository, StorageError};
use doujin_thumbnails::{ThumbnailConfig, ThumbnailGenerationSuccess};
use rusqlite::Connection;

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
        .save_application_settings(Some(stored_reader.clone()), 360, 480, 85)
        .expect("save overridden settings");

    assert_eq!(Some(environment_reader), saved.settings.reader_path);
    assert_eq!(500, saved.settings.thumbnail_width);
    assert_eq!(600, saved.settings.thumbnail_height);
    assert_eq!(90, saved.settings.thumbnail_quality);
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
        .save_application_settings(None, 480, 640, 100)
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
