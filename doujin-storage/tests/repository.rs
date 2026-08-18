use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use doujin_parser::PARSER_VERSION;
use doujin_parser::domain::{Authors, Classification, Parody, ParseInput};
use doujin_parser::parser::parse_filename;
use doujin_scanner::{FilenameNormalization, PendingCollection, SourceKind};
use doujin_storage::canonical::{CanonicalMappingEvidence, EntityKind};
use doujin_storage::collections::{
    CollectionFilters, CollectionQuery, CollectionRootSnapshot, CollectionSort,
    MissingMetadataField, ReviewQueueKind, ReviewQueueQuery, SortDirection,
};
use doujin_storage::consolidation::{ConsolidationChoice, ConsolidationResolution};
use doujin_storage::duplicates::{
    DUPLICATE_FINGERPRINT_ALGORITHM_VERSION, DuplicateFingerprint, DuplicateLevel,
    DuplicateScanItemStatus, DuplicateScanStatus,
};
use doujin_storage::external_search_batches::{
    ExternalSearchBatchItemOutcome, ExternalSearchBatchStrategy, NewExternalSearchBatchItem,
};
use doujin_storage::jobs::{
    ExternalSearchCompletionStatus, ExternalSearchErrorKind, ExternalSearchJobIssue,
    ExternalSearchJobStatus, ExternalSearchJobSummary,
};
use doujin_storage::lifecycle::{CandidateDecision, CollectionStatus, DeleteMode, LocationStatus};
use doujin_storage::metadata::{
    ConfidenceEvidence, ExternalCandidate, ExternalCandidateOutcome, ExternalSearchDisposition,
    ExternalTag, ExternalTagOutcome, MetadataAssertionDecision, MetadataAssertionStatus,
    MetadataField, MetadataSource, MetadataValue,
};
use doujin_storage::saved_views::{SavedViewLayout, SavedViewQuery};
use doujin_storage::statistics::CollectionFacet;
use doujin_storage::thumbnails::{
    BACKGROUND_THUMBNAIL_PRIORITY, ThumbnailErrorKind, ThumbnailStatus,
};
use doujin_storage::vocabulary::VocabularyField;
use doujin_storage::{CatalogRepository, IngestOutcome, StorageError, path_key};
use rusqlite::Connection;

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
            "doujin-storage-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test root");
        Self { path }
    }

    fn pending(&self, filename: &str) -> PendingCollection {
        let path = self.path.join(filename);
        fs::write(&path, b"zip placeholder").expect("create zip");
        PendingCollection {
            folder: self.path.clone(),
            path,
            root_path: self.path.clone(),
            root_label: "測試歸檔".to_owned(),
            source: SourceKind::Archive,
            parser_version: PARSER_VERSION.to_owned(),
            parsed: parse_filename(&ParseInput {
                filename: filename.to_owned(),
                parody_evidence: Vec::new(),
            }),
            filename_normalization: FilenameNormalization::Unchanged,
        }
    }

    fn database(&self) -> PathBuf {
        self.path.join("catalog.db")
    }

    fn pending_under(
        &self,
        root_name: &str,
        source: SourceKind,
        filename: &str,
    ) -> PendingCollection {
        let root = self.path.join(root_name);
        fs::create_dir_all(&root).expect("create nested test root");
        let path = root.join(filename);
        fs::create_dir_all(path.parent().expect("pending parent")).expect("create pending parent");
        fs::write(&path, b"zip placeholder").expect("create nested zip");
        PendingCollection {
            folder: path.parent().expect("pending folder").to_owned(),
            path,
            root_path: root,
            root_label: format!("測試來源 {root_name}"),
            source,
            parser_version: PARSER_VERSION.to_owned(),
            parsed: parse_filename(&ParseInput {
                filename: filename.to_owned(),
                parody_evidence: Vec::new(),
            }),
            filename_normalization: FilenameNormalization::Unchanged,
        }
    }
}

impl Drop for TestTree {
    fn drop(&mut self) {
        if self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("doujin-storage-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn confidence(total: f64, exact_identifier: bool) -> ConfidenceEvidence {
    ConfidenceEvidence {
        total,
        source_reliability: 0.95,
        identifier_match: if exact_identifier { 1.0 } else { 0.4 },
        string_similarity: 0.9,
        rule_certainty: 0.9,
        reliable_identifier_exact_match: exact_identifier,
        reason: "測試用外部來源證據".to_owned(),
    }
}

fn mapping_evidence(reason: &str) -> CanonicalMappingEvidence {
    CanonicalMappingEvidence {
        source_reference: Some("test:canonical".to_owned()),
        reason: reason.to_owned(),
    }
}

fn duplicate_fingerprint(collection_id: i64, source: &str, content: char) -> DuplicateFingerprint {
    let hash = content.to_string().repeat(64);
    DuplicateFingerprint {
        collection_id,
        source_fingerprint: source.to_owned(),
        algorithm_version: DUPLICATE_FINGERPRINT_ALGORITHM_VERSION.to_owned(),
        source_size: 4096,
        file_sha256: Some(hash.clone()),
        archive_entry_count: 2,
        image_count: 2,
        content_fingerprint: hash.clone(),
        page_hashes: vec![hash.clone(), hash],
        perceptual_hashes: None,
        calculated_at: None,
    }
}

#[test]
fn migration_enables_required_sqlite_features() {
    let repository = CatalogRepository::open_in_memory().expect("open catalog");

    assert_eq!(19, repository.schema_version().expect("schema version"));
    assert!(repository.foreign_keys_enabled().expect("foreign keys"));
    assert!(
        repository
            .table_is_strict("collections")
            .expect("STRICT table")
    );
    assert!(
        repository
            .sqlite_version()
            .expect("SQLite version")
            .split('.')
            .next()
            .is_some_and(|major| major == "3")
    );
    assert!(
        repository
            .search_titles("feature_probe")
            .expect("FTS5 query")
            .is_empty()
    );
}

#[test]
fn duplicate_scan_job_persists_101_items_bounds_claims_and_recovers_running_items() {
    let tree = TestTree::new("duplicate-scan-persistence");
    let database = tree.database();
    let mut repository = CatalogRepository::open(&database).expect("open catalog");
    let mut collection_ids = Vec::new();
    for index in 0..101 {
        let pending = tree.pending(&format!("[Circle {index:03}] duplicate {index:03}.zip"));
        repository
            .ingest_collection(&pending)
            .expect("ingest collection");
        collection_ids.push(
            repository
                .collection_id_for_current_path(&pending.path)
                .expect("lookup collection")
                .expect("collection ID"),
        );
    }
    let job = repository
        .create_duplicate_scan_job(&collection_ids, 2)
        .expect("create duplicate scan");
    assert_eq!(101, job.total);
    assert_eq!(101, job.pending);
    assert_eq!(2, job.concurrency_limit);
    drop(repository);

    let mut repository = CatalogRepository::open(&database).expect("reopen catalog");
    let persisted = repository
        .duplicate_scan_job(job.id)
        .expect("persistent duplicate scan");
    assert_eq!(101, persisted.pending);
    let first = repository
        .claim_duplicate_scan_item()
        .expect("claim first")
        .expect("first item");
    let second = repository
        .claim_duplicate_scan_item()
        .expect("claim second")
        .expect("second item");
    assert_ne!(first.collection_id, second.collection_id);
    assert_eq!(DuplicateScanItemStatus::Running, first.status);
    assert!(
        repository
            .claim_duplicate_scan_item()
            .expect("bounded third claim")
            .is_none()
    );
    drop(repository);

    let mut repository = CatalogRepository::open(&database).expect("reopen interrupted catalog");
    assert_eq!(
        2,
        repository
            .recover_interrupted_duplicate_scan_items()
            .expect("recover interrupted items")
    );
    let recovered = repository
        .duplicate_scan_job(job.id)
        .expect("recovered job");
    assert_eq!(101, recovered.pending);
    assert_eq!(0, recovered.running);
}

#[test]
fn duplicate_fingerprint_cache_reuses_only_matching_source_and_algorithm() {
    let tree = TestTree::new("duplicate-cache");
    let pending = tree.pending("[Circle] duplicate cache.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .collection_id_for_current_path(&pending.path)
        .expect("lookup collection")
        .expect("collection ID");
    let fingerprint = duplicate_fingerprint(collection_id, "source:v1", 'a');
    repository
        .upsert_duplicate_fingerprint(&fingerprint)
        .expect("save fingerprint");

    assert!(
        repository
            .cached_duplicate_fingerprint(
                collection_id,
                "source:v1",
                DUPLICATE_FINGERPRINT_ALGORITHM_VERSION,
            )
            .expect("reuse cache")
            .is_some()
    );
    assert!(
        repository
            .cached_duplicate_fingerprint(
                collection_id,
                "source:v2",
                DUPLICATE_FINGERPRINT_ALGORITHM_VERSION,
            )
            .expect("source invalidation")
            .is_none()
    );
    assert!(
        repository
            .cached_duplicate_fingerprint(collection_id, "source:v1", "sha256-pages-v2")
            .expect("algorithm invalidation")
            .is_none()
    );
}

#[test]
fn duplicate_failures_retry_and_pair_decisions_are_fingerprint_bound() {
    let tree = TestTree::new("duplicate-decisions");
    let first = tree.pending("[Circle] duplicate first.zip");
    let second = tree.pending("[Circle] duplicate second.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.ingest_collection(&first).expect("ingest first");
    repository
        .ingest_collection(&second)
        .expect("ingest second");
    let first_id = repository
        .collection_id_for_current_path(&first.path)
        .expect("lookup first")
        .expect("first ID");
    let second_id = repository
        .collection_id_for_current_path(&second.path)
        .expect("lookup second")
        .expect("second ID");
    let job = repository
        .create_duplicate_scan_job(&[first_id], 1)
        .expect("create scan");
    let item = repository
        .claim_duplicate_scan_item()
        .expect("claim item")
        .expect("scan item");
    let failed = repository
        .fail_duplicate_scan_item(job.id, item.collection_id, "invalid_archive", "ZIP 已損毀")
        .expect("record failure");
    assert_eq!(DuplicateScanStatus::CompletedWithErrors, failed.status);
    assert_eq!(1, failed.failed);
    let failures = repository
        .duplicate_scan_failures(job.id)
        .expect("locate duplicate failure");
    assert_eq!(1, failures.len());
    assert_eq!(item.collection_id, failures[0].collection_id);
    assert_eq!(item.path, failures[0].path);
    assert_eq!(Some("invalid_archive"), failures[0].error_kind.as_deref());
    assert_eq!(1, failures[0].attempts);
    let retried = repository
        .retry_failed_duplicate_scan_items(job.id)
        .expect("retry failure");
    assert_eq!(DuplicateScanStatus::Running, retried.status);
    assert_eq!(1, retried.pending);
    let retried_item = repository
        .claim_duplicate_scan_item()
        .expect("claim retry")
        .expect("retried item");
    assert_eq!(2, retried_item.attempts);
    let completed = repository
        .complete_duplicate_scan_item(
            job.id,
            &duplicate_fingerprint(first_id, "source:first", 'b'),
            false,
        )
        .expect("complete retry");
    assert_eq!(DuplicateScanStatus::Completed, completed.status);

    repository
        .exclude_duplicate_pair(first_id, "fp:first:v1", second_id, "fp:second:v1")
        .expect("exclude pair");
    assert!(
        repository
            .duplicate_pair_is_excluded(first_id, "fp:first:v1", second_id, "fp:second:v1")
            .expect("read exclusion")
    );
    assert!(
        !repository
            .duplicate_pair_is_excluded(first_id, "fp:first:v2", second_id, "fp:second:v1")
            .expect("changed fingerprint re-evaluation")
    );
    repository
        .confirm_duplicate_pair(second_id, "fp:second:v1", first_id, "fp:first:v1")
        .expect("confirm pair with reverse input order");
    assert!(
        repository
            .duplicate_pair_is_confirmed(first_id, "fp:first:v1", second_id, "fp:second:v1")
            .expect("read review")
    );
    assert!(
        !repository
            .duplicate_pair_is_confirmed(first_id, "fp:first:v2", second_id, "fp:second:v1")
            .expect("changed fingerprint not reviewed")
    );
}

#[test]
fn duplicate_candidates_prioritize_levels_require_conservative_probable_evidence_and_honor_decisions()
 {
    let tree = TestTree::new("duplicate-candidates");
    let filenames = [
        "[Exact Circle] Exact Work A.zip",
        "[Exact Circle] Exact Work B.zip",
        "[Content Circle] Content Work A.zip",
        "[Content Circle] Content Work B.zip",
        "[Same Circle (Shared Author)] Probable Work.zip",
        "[Same Circle (Shared Author)] Probable Work [DL版].zip",
        "[Different One] Title Alone.zip",
        "[Different Two] Title Alone.zip",
        "[RJ407766][Identifier One] First Edition.zip",
        "[RJ407766][Identifier Two] Completely Renamed.zip",
    ];
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut ids = Vec::new();
    for filename in filenames {
        let pending = tree.pending(filename);
        repository
            .ingest_collection(&pending)
            .expect("ingest candidate");
        ids.push(
            repository
                .collection_id_for_current_path(&pending.path)
                .expect("lookup candidate")
                .expect("candidate ID"),
        );
    }
    let mut fingerprints = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            duplicate_fingerprint(
                *id,
                &format!("source:{index}"),
                char::from(b'a' + index as u8),
            )
        })
        .collect::<Vec<_>>();
    fingerprints[1].file_sha256 = fingerprints[0].file_sha256.clone();
    fingerprints[1].content_fingerprint = fingerprints[0].content_fingerprint.clone();
    fingerprints[1].page_hashes = fingerprints[0].page_hashes.clone();
    fingerprints[3].content_fingerprint = fingerprints[2].content_fingerprint.clone();
    fingerprints[3].page_hashes = fingerprints[2].page_hashes.clone();
    fingerprints[5].image_count = fingerprints[4].image_count + 1;
    fingerprints[5].page_hashes.push("f".repeat(64));
    fingerprints[9].image_count = 20;
    fingerprints[9].page_hashes = vec!["9".repeat(64); 20];
    for fingerprint in &fingerprints {
        repository
            .upsert_duplicate_fingerprint(fingerprint)
            .expect("save candidate fingerprint");
    }

    let candidates = repository.duplicate_candidates().expect("candidate query");
    assert_eq!(DuplicateLevel::Exact, candidates[0].level);
    assert_eq!(
        (ids[0], ids[1]),
        (
            candidates[0].left.collection.id,
            candidates[0].right.collection.id
        )
    );
    assert_eq!(DuplicateLevel::Content, candidates[1].level);
    assert_eq!(
        (ids[2], ids[3]),
        (
            candidates[1].left.collection.id,
            candidates[1].right.collection.id
        )
    );
    assert!(candidates.iter().any(|candidate| {
        candidate.level == DuplicateLevel::Probable
            && candidate.left.collection.id == ids[4]
            && candidate.right.collection.id == ids[5]
            && candidate
                .reasons
                .iter()
                .any(|reason| reason.contains("作者相同"))
    }));
    assert!(candidates.iter().any(|candidate| {
        candidate.level == DuplicateLevel::Probable
            && candidate.left.collection.id == ids[8]
            && candidate.right.collection.id == ids[9]
            && candidate
                .reasons
                .iter()
                .any(|reason| reason.contains("RJ:RJ407766"))
    }));
    assert!(
        !candidates.iter().any(|candidate| {
            candidate.left.collection.id == ids[6] && candidate.right.collection.id == ids[7]
        }),
        "title alone must not create a probable candidate"
    );
    assert!(candidates[0].left.file_size > 0);
    assert!(candidates[0].left.page_count > 0);
    assert!(candidates[0].left.metadata_completeness > 0);
    assert_eq!(0, candidates[0].left.manual_assertion_count);
    assert!(candidates[0].left.collection.root.is_some());

    let exact_left_identity = candidates[0].left.fingerprint_identity.clone();
    let exact_right_identity = candidates[0].right.fingerprint_identity.clone();
    repository
        .confirm_duplicate_pair(ids[0], &exact_left_identity, ids[1], &exact_right_identity)
        .expect("confirm exact pair");
    let reviewed = repository
        .duplicate_candidates()
        .expect("reviewed candidates");
    assert!(reviewed.iter().any(|candidate| {
        candidate.left.collection.id == ids[0]
            && candidate.right.collection.id == ids[1]
            && candidate.reviewed
    }));

    repository
        .exclude_duplicate_pair(ids[0], &exact_left_identity, ids[1], &exact_right_identity)
        .expect("exclude exact pair");
    let excluded = repository
        .duplicate_candidates()
        .expect("excluded candidates");
    assert!(!excluded.iter().any(|candidate| {
        candidate.left.collection.id == ids[0] && candidate.right.collection.id == ids[1]
    }));

    fingerprints[0].source_fingerprint = "source:changed".to_owned();
    repository
        .upsert_duplicate_fingerprint(&fingerprints[0])
        .expect("change fingerprint identity");
    let reevaluated = repository
        .duplicate_candidates()
        .expect("re-evaluated candidates");
    assert!(reevaluated.iter().any(|candidate| {
        candidate.left.collection.id == ids[0]
            && candidate.right.collection.id == ids[1]
            && !candidate.reviewed
    }));
}

#[test]
fn work_basket_persists_large_idempotent_batches_and_cascades_hard_deletes() {
    let tree = TestTree::new("work-basket");
    let database = tree.database();
    let mut repository = CatalogRepository::open(&database).expect("open catalog");
    let mut pending = Vec::new();
    let mut collection_ids = Vec::new();
    for index in 0..120 {
        let item = tree.pending(&format!("[Circle {index:03}] basket item {index:03}.zip"));
        repository
            .ingest_collection(&item)
            .expect("ingest basket collection");
        collection_ids.push(
            repository
                .collection_id_for_current_path(&item.path)
                .expect("collection lookup")
                .expect("collection ID"),
        );
        pending.push(item);
    }

    let basket = repository
        .add_to_work_basket(1, &collection_ids)
        .expect("add large basket batch");
    assert_eq!(120, basket.items.len());
    let repeated = repository
        .add_to_work_basket(1, &collection_ids)
        .expect("repeat basket batch");
    assert_eq!(120, repeated.items.len());
    assert_eq!(
        120,
        repository.work_baskets().expect("basket list")[0].count
    );

    let unrelated_query = repository
        .collections(&CollectionQuery {
            search: Some("no collection matches this query".to_owned()),
            ..CollectionQuery::default()
        })
        .expect("change collection query");
    assert_eq!(0, unrelated_query.total);
    assert_eq!(
        120,
        repository
            .work_basket(1)
            .expect("basket after query")
            .items
            .len()
    );
    drop(repository);

    let mut repository = CatalogRepository::open(&database).expect("reopen catalog");
    assert_eq!(
        120,
        repository
            .work_basket(1)
            .expect("persistent basket")
            .items
            .len()
    );

    let deleted_id = collection_ids[0];
    let operation = repository
        .begin_delete(deleted_id, DeleteMode::Permanent)
        .expect("begin permanent delete");
    fs::remove_file(&pending[0].path).expect("remove collection file");
    repository
        .complete_file_operation(operation.id)
        .expect("complete permanent delete");
    let basket = repository.work_basket(1).expect("basket after hard delete");
    assert_eq!(119, basket.items.len());
    assert!(
        !basket
            .items
            .iter()
            .any(|item| item.collection.id == deleted_id)
    );

    assert!(
        repository
            .remove_from_work_basket(1, collection_ids[1])
            .expect("remove item")
    );
    assert!(
        !repository
            .remove_from_work_basket(1, collection_ids[1])
            .expect("repeat remove")
    );
    assert_eq!(118, repository.clear_work_basket(1).expect("clear basket"));
    assert!(
        repository
            .work_basket(1)
            .expect("empty basket")
            .items
            .is_empty()
    );
}

#[test]
fn saved_views_persist_allowlisted_query_rules_and_support_explicit_crud() {
    let tree = TestTree::new("saved-views");
    let database = tree.database();
    let mut repository = CatalogRepository::open(&database).expect("open catalog");
    let query = CollectionQuery {
        search: Some("C106 blue archive".to_owned()),
        sort: CollectionSort::Updated,
        direction: SortDirection::Ascending,
        filters: CollectionFilters {
            source: Some(SourceKind::Downloads),
            event: Some("C106".to_owned()),
            tags: vec!["待整理".to_owned(), "會場限定".to_owned()],
            missing: vec![MissingMetadataField::Parody],
            ..CollectionFilters::default()
        },
        ..CollectionQuery::default()
    };
    let saved_query = SavedViewQuery::from_collection_query(&query, SavedViewLayout::List);

    let created = repository
        .create_saved_view("C106 待整理", &saved_query, true)
        .expect("create saved view");
    assert_eq!("C106 待整理", created.name);
    assert_eq!(saved_query, created.query);
    assert!(created.pinned);
    assert!(matches!(
        repository
            .create_saved_view("c106 待整理", &saved_query, false)
            .expect_err("name is unique without ASCII case"),
        StorageError::SavedViewNameConflict(_)
    ));
    drop(repository);

    let mut repository = CatalogRepository::open(&database).expect("reopen catalog");
    let persisted = repository.saved_view(created.id).expect("persisted view");
    assert_eq!(saved_query, persisted.query);
    assert_eq!(1, repository.saved_views().expect("list views").len());

    let renamed = repository
        .update_saved_view(created.id, "C106 補原作", &saved_query, false)
        .expect("rename and update");
    assert_eq!("C106 補原作", renamed.name);
    assert!(!renamed.pinned);
    repository
        .delete_saved_view(created.id)
        .expect("delete saved view");
    assert!(matches!(
        repository
            .saved_view(created.id)
            .expect_err("deleted view is gone"),
        StorageError::SavedViewNotFound(id) if id == created.id
    ));
}

#[test]
fn library_roots_can_be_listed_deactivated_and_reactivated() {
    let tree = TestTree::new("library-roots");
    let root = tree.path.join("library");
    fs::create_dir(&root).expect("create library root");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");

    let root_id = repository
        .register_library_root(&root, SourceKind::Downloads, "下載區")
        .expect("register root");
    let roots = repository.library_roots().expect("list roots");
    assert_eq!(1, roots.len());
    assert_eq!(root_id, roots[0].id);
    assert_eq!(root, roots[0].path);
    assert_eq!(SourceKind::Downloads, roots[0].source);
    assert!(roots[0].active);
    assert_eq!(
        1,
        repository.active_scan_roots().expect("active roots").len()
    );

    let deactivated = repository
        .deactivate_library_root(root_id)
        .expect("deactivate root");
    assert!(!deactivated.active);
    assert!(
        repository
            .active_scan_roots()
            .expect("active roots")
            .is_empty()
    );

    let reactivated_id = repository
        .register_library_root(&root, SourceKind::Archive, "歸檔區")
        .expect("reactivate root");
    assert_eq!(root_id, reactivated_id);
    let reactivated = repository.library_root(root_id).expect("reactivated root");
    assert!(reactivated.active);
    assert_eq!(SourceKind::Archive, reactivated.source);
    assert_eq!("歸檔區", reactivated.label);
    assert_eq!(1, repository.library_roots().expect("list roots").len());
}

#[test]
fn library_roots_can_be_edited_without_changing_identity() {
    let tree = TestTree::new("edit-library-root");
    let original = tree.path.join("original");
    let updated = tree.path.join("updated");
    let other = tree.path.join("other");
    fs::create_dir_all(&original).expect("create original root");
    fs::create_dir_all(&updated).expect("create updated root");
    fs::create_dir_all(&other).expect("create other root");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    let root_id = repository
        .register_library_root(&original, SourceKind::Downloads, "下載區")
        .expect("register root");
    let other_id = repository
        .register_library_root(&other, SourceKind::Archive, "其他")
        .expect("register other root");

    let edited = repository
        .update_library_root(root_id, &updated, SourceKind::Archive, "  典藏區  ")
        .expect("edit root");
    assert_eq!(root_id, edited.id);
    assert_eq!(updated, edited.path);
    assert_eq!(SourceKind::Archive, edited.source);
    assert_eq!("典藏區", edited.label);

    let collision = repository
        .update_library_root(root_id, &other, SourceKind::Archive, "衝突")
        .expect_err("reject duplicate path");
    assert!(matches!(
        collision,
        doujin_storage::StorageError::InvalidLibraryRoot(_)
    ));
    assert_eq!(
        other_id,
        repository.library_root(other_id).expect("other root").id
    );

    repository
        .deactivate_library_root(root_id)
        .expect("deactivate root");
    fs::remove_dir_all(&updated).expect("remove updated root");
    let missing = repository
        .reactivate_library_root(root_id)
        .expect_err("reject missing root");
    assert!(matches!(
        missing,
        doujin_storage::StorageError::InvalidLibraryRoot(_)
    ));
    fs::create_dir_all(&updated).expect("restore updated root");
    assert!(
        repository
            .reactivate_library_root(root_id)
            .expect("reactivate root")
            .active
    );
}

#[test]
fn external_search_jobs_deduplicate_persist_results_and_schedule_typed_retries() {
    let tree = TestTree::new("external-search-jobs");
    let first = tree.pending("[circle] first.zip");
    let second = tree.pending("[circle] second.zip");
    let third = tree.pending("[circle] third.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    for pending in [&first, &second, &third] {
        repository
            .ingest_collection(pending)
            .expect("ingest collection");
    }
    let collection_id = |pending: &doujin_scanner::PendingCollection| {
        repository
            .collection_id_for_current_path(&pending.path)
            .expect("collection lookup")
            .expect("collection ID")
    };
    let first_id = collection_id(&first);
    let second_id = collection_id(&second);
    let third_id = collection_id(&third);

    let enqueued = repository
        .enqueue_external_search(
            first_id,
            &[
                MetadataField::Circle,
                MetadataField::Title,
                MetadataField::Circle,
            ],
        )
        .expect("enqueue first search");
    assert!(enqueued.created);
    assert_eq!(
        vec![MetadataField::Title, MetadataField::Circle],
        enqueued.job.fields
    );
    let duplicate = repository
        .enqueue_external_search(first_id, &[MetadataField::Event])
        .expect("deduplicate active search");
    assert!(!duplicate.created);
    assert_eq!(enqueued.job.id, duplicate.job.id);
    assert_eq!(enqueued.job.fields, duplicate.job.fields);
    assert_eq!(
        vec![enqueued.job.id],
        repository
            .due_external_search_jobs(10)
            .expect("due jobs")
            .into_iter()
            .map(|job| job.id)
            .collect::<Vec<_>>()
    );

    let running = repository
        .start_external_search_job(enqueued.job.id)
        .expect("start first search");
    assert_eq!(ExternalSearchJobStatus::Running, running.status);
    assert_eq!(1, running.attempts);
    let retrying = repository
        .fail_external_search_job(
            running.id,
            ExternalSearchErrorKind::Network,
            "temporary network failure",
            None,
        )
        .expect("schedule network retry");
    assert_eq!(ExternalSearchJobStatus::Pending, retrying.status);
    assert_eq!(Some(ExternalSearchErrorKind::Network), retrying.error_kind);
    let network_retry_at = retrying.next_retry_at.clone().expect("network retry time");
    assert!(
        repository
            .due_external_search_jobs(10)
            .expect("future retry is not due")
            .is_empty()
    );
    assert!(matches!(
        repository
            .start_external_search_job(retrying.id)
            .expect_err("future retry cannot start early"),
        StorageError::ExternalSearchJobUnavailable(id) if id == retrying.id
    ));

    let rate_limited = repository
        .enqueue_external_search(second_id, &[MetadataField::Title])
        .expect("enqueue rate-limited search")
        .job;
    repository
        .start_external_search_job(rate_limited.id)
        .expect("start rate-limited search");
    let rate_limited = repository
        .fail_external_search_job(
            rate_limited.id,
            ExternalSearchErrorKind::RateLimited,
            "provider rate limit",
            None,
        )
        .expect("schedule rate-limit retry");
    assert!(
        rate_limited
            .next_retry_at
            .as_deref()
            .expect("rate-limit retry time")
            > network_retry_at.as_str()
    );
    assert_eq!(
        None,
        ExternalSearchErrorKind::InvalidResponse.retry_delay_seconds(1)
    );

    let partial = repository
        .enqueue_external_search(third_id, &[MetadataField::Title, MetadataField::Authors])
        .expect("enqueue partial search")
        .job;
    repository
        .start_external_search_job(partial.id)
        .expect("start partial search");
    let partial = repository
        .complete_external_search_job(
            partial.id,
            ExternalSearchCompletionStatus::Partial,
            &ExternalSearchJobSummary {
                candidates_received: 1,
                tags_received: 0,
                tags_applied: 0,
                auto_applied: 0,
                suggestions: 1,
                suggestion_assertion_ids: Vec::new(),
                search_only: 0,
                issues: vec![ExternalSearchJobIssue {
                    field: Some(MetadataField::Authors),
                    kind: "provider_field_error".to_owned(),
                    message: "authors lookup failed".to_owned(),
                }],
            },
        )
        .expect("complete partial search");
    assert_eq!(ExternalSearchJobStatus::Partial, partial.status);
    assert_eq!(None, partial.next_retry_at);
    let result: serde_json::Value = serde_json::from_str(
        partial
            .result_json
            .as_deref()
            .expect("partial result summary"),
    )
    .expect("decode result summary");
    assert_eq!(1, result["suggestions"]);
    assert_eq!("authors", result["issues"][0]["field"]);

    let interrupted = repository
        .enqueue_external_search(third_id, &[MetadataField::Event])
        .expect("enqueue interruptible search")
        .job;
    let interrupted = repository
        .start_external_search_job(interrupted.id)
        .expect("start interruptible search");
    assert_eq!(ExternalSearchJobStatus::Running, interrupted.status);
    assert_eq!(1, interrupted.attempts);
    assert_eq!(
        1,
        repository
            .recover_interrupted_external_search_jobs()
            .expect("recover interrupted jobs")
    );
    assert_eq!(
        0,
        repository
            .recover_interrupted_external_search_jobs()
            .expect("recovery is idempotent")
    );
    let recovered = repository
        .external_search_job(interrupted.id)
        .expect("recovered job");
    assert_eq!(ExternalSearchJobStatus::Pending, recovered.status);
    assert_eq!(
        Some(ExternalSearchErrorKind::WorkerInterrupted),
        recovered.error_kind
    );
    assert_eq!(None, recovered.next_retry_at);
    assert_eq!(1, recovered.attempts);
    assert_eq!(
        vec![recovered.id],
        repository
            .due_external_search_jobs(10)
            .expect("recovered job is immediately due")
            .into_iter()
            .map(|job| job.id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn version_seventeen_external_search_activity_uses_selected_assertion_time_for_legacy_jobs() {
    let tree = TestTree::new("upgrade-v17-external-activity");
    let database = tree.database();
    let first = tree.pending("[circle (Existing Author)] legacy changed.zip");
    let second = tree.pending("[circle (Existing Author)] legacy unchanged.zip");
    let mut repository = CatalogRepository::open(&database).expect("create current catalog");
    for pending in [&first, &second] {
        repository
            .ingest_collection(pending)
            .expect("ingest legacy activity collection");
    }
    let collection_id = |pending: &doujin_scanner::PendingCollection| {
        repository
            .collection_id_for_current_path(&pending.path)
            .expect("legacy collection lookup")
            .expect("legacy collection ID")
    };
    let changed_id = collection_id(&first);
    let unchanged_id = collection_id(&second);

    let changed_job = repository
        .enqueue_external_search(changed_id, &[MetadataField::Authors])
        .expect("enqueue changed legacy job")
        .job;
    repository
        .start_external_search_job(changed_job.id)
        .expect("start changed legacy job");
    repository
        .fail_external_search_job(
            changed_job.id,
            ExternalSearchErrorKind::NoMatch,
            "legacy no match",
            None,
        )
        .expect("fail changed legacy job");
    repository
        .set_manual_value(
            changed_id,
            MetadataField::Authors,
            MetadataValue::Authors(Authors {
                raw: Some("Later Author".to_owned()),
                values: vec!["Later Author".to_owned()],
            }),
        )
        .expect("set post-job legacy authors");

    repository
        .set_manual_value(
            unchanged_id,
            MetadataField::Authors,
            MetadataValue::Authors(Authors {
                raw: Some("Existing Author".to_owned()),
                values: vec!["Existing Author".to_owned()],
            }),
        )
        .expect("set pre-job legacy authors");
    let unchanged_job = repository
        .enqueue_external_search(unchanged_id, &[MetadataField::Authors])
        .expect("enqueue unchanged legacy job")
        .job;
    repository
        .start_external_search_job(unchanged_job.id)
        .expect("start unchanged legacy job");
    repository
        .fail_external_search_job(
            unchanged_job.id,
            ExternalSearchErrorKind::NoMatch,
            "legacy no match",
            None,
        )
        .expect("fail unchanged legacy job");
    drop(repository);

    let connection = Connection::open(&database).expect("rewind catalog to v17");
    connection
        .execute(
            "UPDATE background_jobs
             SET payload_json = json_remove(payload_json, '$.selection_baseline'),
                 created_at = '2026-08-18T00:00:00.000Z'
             WHERE id = ?1",
            [changed_job.id],
        )
        .expect("make changed job legacy");
    connection
        .execute(
            "UPDATE metadata_assertions
             SET created_at = CASE
                 WHEN source_kind = 'manual' THEN '2026-08-18T00:00:01.000Z'
                 ELSE '2026-08-17T00:00:00.000Z'
             END
             WHERE collection_id = ?1 AND field_name = 'authors'",
            [changed_id],
        )
        .expect("date changed assertion after job and prior assertion before job");
    connection
        .execute(
            "UPDATE background_jobs
             SET payload_json = json_remove(payload_json, '$.selection_baseline'),
                 created_at = '2026-08-18T00:00:01.000Z'
             WHERE id = ?1",
            [unchanged_job.id],
        )
        .expect("make unchanged job legacy");
    connection
        .execute(
            "UPDATE metadata_assertions SET created_at = '2026-08-18T00:00:00.000Z'
             WHERE collection_id = ?1 AND field_name = 'authors'",
            [unchanged_id],
        )
        .expect("date unchanged assertion before job");
    connection
        .execute_batch(
            "DROP TABLE external_search_job_resolutions;
             ALTER TABLE application_settings DROP COLUMN library_batch_size;
             DELETE FROM schema_migrations WHERE version IN (18, 19);
             PRAGMA user_version = 17;",
        )
        .expect("rewind external activity migration");
    drop(connection);

    let mut repository =
        CatalogRepository::open(&database).expect("upgrade legacy activity catalog");
    assert_eq!(19, repository.schema_version().expect("upgraded schema"));
    let activity = repository
        .external_search_activity()
        .expect("legacy external activity");
    assert_eq!(1, activity.actionable_count);
    let changed = activity
        .items
        .iter()
        .find(|item| item.job.id == changed_job.id)
        .expect("changed legacy activity");
    assert!(!changed.actionable);
    assert_eq!(
        Some(doujin_storage::jobs::ExternalSearchActivityResolution::MetadataResolved),
        changed.resolution
    );
    let unchanged = activity
        .items
        .iter()
        .find(|item| item.job.id == unchanged_job.id)
        .expect("unchanged legacy activity");
    assert!(unchanged.actionable);
    assert_eq!(vec![MetadataField::Authors], unchanged.unresolved_fields);

    assert!(
        repository
            .clear_manual_value(changed_id, MetadataField::Authors)
            .expect("clear post-job manual authors")
    );
    let restored = repository
        .external_search_activity()
        .expect("activity after restoring pre-job assertion");
    assert_eq!(2, restored.actionable_count);
    let restored_changed = restored
        .items
        .iter()
        .find(|item| item.job.id == changed_job.id)
        .expect("restored legacy activity");
    assert!(restored_changed.actionable);
    assert_eq!(
        vec![MetadataField::Authors],
        restored_changed.unresolved_fields
    );
}

#[test]
fn external_search_batch_persists_100_plus_items_and_aggregates_linked_job_state() {
    let tree = TestTree::new("external-search-batch");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut collection_ids = Vec::new();
    for index in 0..105 {
        let pending = tree.pending(&format!("[circle] batch {index:03}.zip"));
        repository
            .ingest_collection(&pending)
            .expect("ingest batch collection");
        collection_ids.push(
            repository
                .collection_id_for_current_path(&pending.path)
                .expect("collection lookup")
                .expect("collection ID"),
        );
    }

    let pending_job = repository
        .enqueue_external_search(collection_ids[0], &[MetadataField::Title])
        .expect("enqueue pending")
        .job;
    let running_job = repository
        .enqueue_external_search(collection_ids[1], &[MetadataField::Authors])
        .expect("enqueue running")
        .job;
    repository
        .start_external_search_job(running_job.id)
        .expect("start running");
    let succeeded_job = repository
        .enqueue_external_search(collection_ids[2], &[MetadataField::Parody])
        .expect("enqueue succeeded")
        .job;
    repository
        .start_external_search_job(succeeded_job.id)
        .expect("start succeeded");
    repository
        .complete_external_search_job(
            succeeded_job.id,
            ExternalSearchCompletionStatus::Succeeded,
            &ExternalSearchJobSummary {
                candidates_received: 1,
                tags_received: 0,
                tags_applied: 0,
                auto_applied: 0,
                suggestions: 1,
                suggestion_assertion_ids: Vec::new(),
                search_only: 0,
                issues: Vec::new(),
            },
        )
        .expect("complete succeeded");

    let mut items = vec![
        NewExternalSearchBatchItem {
            collection_id: collection_ids[0],
            job_id: Some(pending_job.id),
            outcome: ExternalSearchBatchItemOutcome::Reused,
            fields: vec![MetadataField::Title],
            reason: Some("已有 pending/running 工作".to_owned()),
        },
        NewExternalSearchBatchItem {
            collection_id: collection_ids[1],
            job_id: Some(running_job.id),
            outcome: ExternalSearchBatchItemOutcome::Enqueued,
            fields: vec![MetadataField::Authors],
            reason: None,
        },
        NewExternalSearchBatchItem {
            collection_id: collection_ids[2],
            job_id: Some(succeeded_job.id),
            outcome: ExternalSearchBatchItemOutcome::Enqueued,
            fields: vec![MetadataField::Parody],
            reason: None,
        },
    ];
    items.extend(
        collection_ids[3..104]
            .iter()
            .map(|collection_id| NewExternalSearchBatchItem {
                collection_id: *collection_id,
                job_id: None,
                outcome: ExternalSearchBatchItemOutcome::Unchanged,
                fields: Vec::new(),
                reason: Some("指定欄位已有值".to_owned()),
            }),
    );
    items.push(NewExternalSearchBatchItem {
        collection_id: collection_ids[104],
        job_id: None,
        outcome: ExternalSearchBatchItemOutcome::Skipped,
        fields: vec![MetadataField::Title],
        reason: Some("缺少足夠識別資訊".to_owned()),
    });

    let batch = repository
        .create_external_search_batch(
            ExternalSearchBatchStrategy::OnlyMissing,
            &[
                MetadataField::Title,
                MetadataField::Authors,
                MetadataField::Parody,
            ],
            &items,
        )
        .expect("create persistent batch");
    assert_eq!(105, batch.summary.total);
    assert_eq!(1, batch.summary.pending);
    assert_eq!(1, batch.summary.running);
    assert_eq!(1, batch.summary.succeeded);
    assert_eq!(0, batch.summary.partial);
    assert_eq!(0, batch.summary.failed);
    assert_eq!(1, batch.summary.skipped);
    assert_eq!(101, batch.summary.unchanged);
    assert_eq!(1, batch.summary.reused);
    assert_eq!(105, batch.items.len());
    assert_eq!(
        vec![MetadataField::Title],
        batch.items[0].fields,
        "per-collection requested fields stay traceable"
    );

    let running = repository
        .external_search_job(running_job.id)
        .expect("read running");
    repository
        .complete_external_search_job(
            running.id,
            ExternalSearchCompletionStatus::Partial,
            &ExternalSearchJobSummary {
                candidates_received: 1,
                tags_received: 0,
                tags_applied: 0,
                auto_applied: 0,
                suggestions: 0,
                suggestion_assertion_ids: Vec::new(),
                search_only: 1,
                issues: vec![ExternalSearchJobIssue {
                    field: Some(MetadataField::Authors),
                    kind: "provider_field_error".to_owned(),
                    message: "部分欄位無結果".to_owned(),
                }],
            },
        )
        .expect("complete partial");
    let refreshed = repository
        .external_search_batch(batch.id)
        .expect("refresh dynamic summary");
    assert_eq!(0, refreshed.summary.running);
    assert_eq!(1, refreshed.summary.partial);
    assert_eq!(
        1,
        serde_json::from_str::<serde_json::Value>(
            &repository
                .external_search_job(running.id)
                .expect("read partial")
                .result_json
                .expect("partial result")
        )
        .expect("decode summary")["search_only"],
        "batch references the original evidence-bearing job without promoting search_only"
    );
}

#[test]
fn thumbnail_states_deduplicate_retry_and_require_manual_reset_after_permanent_failure() {
    let tree = TestTree::new("thumbnail-state");
    let pending = tree.pending("[circle] cover test.zip");
    let cache_path = tree.path.join("thumbs").join("1.webp");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .collection_id_for_current_path(&pending.path)
        .expect("collection lookup")
        .expect("collection ID");

    let first = repository
        .request_thumbnail(
            collection_id,
            "source-v1",
            "300x400-q80",
            &cache_path,
            false,
        )
        .expect("request first thumbnail");
    assert!(first.enqueued);
    assert_eq!(ThumbnailStatus::Pending, first.state.status);
    let duplicate = repository
        .request_thumbnail(
            collection_id,
            "source-v1",
            "300x400-q80",
            &cache_path,
            false,
        )
        .expect("deduplicate thumbnail");
    assert!(!duplicate.enqueued);

    let running = repository
        .start_thumbnail(collection_id)
        .expect("start thumbnail");
    assert_eq!(ThumbnailStatus::Running, running.status);
    assert_eq!(1, running.attempts);
    let retrying = repository
        .fail_thumbnail(
            collection_id,
            ThumbnailErrorKind::SourceIo,
            "source temporarily unavailable",
        )
        .expect("schedule retry");
    assert_eq!(ThumbnailStatus::Pending, retrying.status);
    assert_eq!(Some(ThumbnailErrorKind::SourceIo), retrying.error_kind);
    assert!(retrying.next_retry_at.is_some());
    assert!(
        repository
            .due_thumbnails(10)
            .expect("due thumbnails")
            .is_empty()
    );
    assert!(matches!(
        repository
            .start_thumbnail(collection_id)
            .expect_err("future retry cannot start early"),
        StorageError::ThumbnailStateUnavailable(id) if id == collection_id
    ));

    let changed = repository
        .request_thumbnail(
            collection_id,
            "source-v2",
            "300x400-q80",
            &cache_path,
            false,
        )
        .expect("changed source requeues thumbnail");
    assert!(changed.enqueued);
    assert_eq!(0, changed.state.attempts);
    assert_eq!(None, changed.state.error_kind);
    repository
        .start_thumbnail(collection_id)
        .expect("start permanent failure");
    let failed = repository
        .fail_thumbnail(
            collection_id,
            ThumbnailErrorKind::InvalidArchive,
            "not a readable ZIP archive",
        )
        .expect("record permanent failure");
    assert_eq!(ThumbnailStatus::Failed, failed.status);
    assert_eq!(None, failed.next_retry_at);
    assert!(
        repository
            .due_thumbnails(10)
            .expect("due thumbnails")
            .is_empty()
    );
    let unchanged = repository
        .request_thumbnail(
            collection_id,
            "source-v2",
            "300x400-q80",
            &cache_path,
            false,
        )
        .expect("permanent failure stays failed");
    assert!(!unchanged.enqueued);
    assert_eq!(ThumbnailStatus::Failed, unchanged.state.status);

    let reset = repository
        .reset_thumbnail(collection_id, "source-v2", "300x400-q80", &cache_path)
        .expect("manual rebuild resets failure");
    assert_eq!(ThumbnailStatus::Pending, reset.status);
    assert_eq!(0, reset.attempts);
    repository
        .start_thumbnail(collection_id)
        .expect("start rebuilt thumbnail");
    let ready = repository
        .complete_thumbnail(collection_id, 120, 160)
        .expect("complete thumbnail");
    assert_eq!(ThumbnailStatus::Ready, ready.status);
    assert_eq!(Some(120), ready.generated_width);
    assert_eq!(Some(160), ready.generated_height);
    let missing_cache = repository
        .request_thumbnail(
            collection_id,
            "source-v2",
            "300x400-q80",
            &cache_path,
            false,
        )
        .expect("missing ready cache requeues thumbnail");
    assert!(missing_cache.enqueued);
    assert_eq!(ThumbnailStatus::Pending, missing_cache.state.status);
}

#[test]
fn thumbnail_priority_orders_interactive_work_before_background_prewarm() {
    let tree = TestTree::new("thumbnail-priority");
    let low = tree.pending("[circle] low.zip");
    let visible = tree.pending("[circle] visible.zip");
    let newest = tree.pending("[circle] newest.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut collection_ids = Vec::new();
    for pending in [&low, &visible, &newest] {
        repository
            .ingest_collection(pending)
            .expect("ingest collection");
        collection_ids.push(
            repository
                .collection_id_for_current_path(&pending.path)
                .expect("collection lookup")
                .expect("collection ID"),
        );
    }

    for (collection_id, priority) in [
        (collection_ids[0], BACKGROUND_THUMBNAIL_PRIORITY),
        (collection_ids[1], 100),
        (collection_ids[2], 200),
    ] {
        repository
            .request_thumbnail_with_priority(
                collection_id,
                "source-v1",
                "300x400-q80",
                &tree
                    .path
                    .join("thumbs")
                    .join(format!("{collection_id}.webp")),
                false,
                priority,
            )
            .expect("request prioritized thumbnail");
    }

    assert_eq!(
        vec![collection_ids[2], collection_ids[1], collection_ids[0]],
        repository
            .due_thumbnails(10)
            .expect("all due thumbnails")
            .into_iter()
            .map(|state| state.collection_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        vec![collection_ids[2], collection_ids[1]],
        repository
            .due_thumbnails_with_min_priority(10, 1)
            .expect("interactive thumbnails")
            .into_iter()
            .map(|state| state.collection_id)
            .collect::<Vec<_>>()
    );

    let promoted = repository
        .request_thumbnail_with_priority(
            collection_ids[0],
            "source-v1",
            "300x400-q80",
            &tree
                .path
                .join("thumbs")
                .join(format!("{}.webp", collection_ids[0])),
            false,
            300,
        )
        .expect("promote background thumbnail");
    assert_eq!(300, promoted.state.priority);
    assert_eq!(
        collection_ids[0],
        repository
            .due_thumbnails(1)
            .expect("promoted thumbnail first")
            .into_iter()
            .next()
            .expect("due thumbnail")
            .collection_id
    );
    assert_eq!(
        None,
        repository
            .next_untracked_thumbnail_collection_id()
            .expect("all thumbnails tracked")
    );
}

#[test]
fn typed_settings_save_atomically_and_requeue_changed_thumbnail_settings() {
    let tree = TestTree::new("application-settings");
    let pending = tree.pending("[circle] settings.zip");
    let cache_path = tree.path.join("cache").join("1.webp");
    let reader_path = tree.path.join("reader.exe");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .collection_id_for_current_path(&pending.path)
        .expect("collection lookup")
        .expect("collection ID");
    repository
        .request_thumbnail(
            collection_id,
            "source-v1",
            "300x400-q80",
            &cache_path,
            false,
        )
        .expect("request thumbnail");
    repository
        .start_thumbnail(collection_id)
        .expect("start thumbnail");
    repository
        .complete_thumbnail(collection_id, 150, 200)
        .expect("complete thumbnail");

    assert_eq!(
        None,
        repository
            .stored_application_settings()
            .expect("empty settings")
    );
    let saved = repository
        .save_application_settings(
            Some(&reader_path),
            360,
            480,
            85,
            "360x480-q85-webp-v1",
            None,
            96,
        )
        .expect("save settings");
    assert_eq!(Some(reader_path), saved.settings.reader_path);
    assert_eq!(360, saved.settings.thumbnail_width);
    assert_eq!(480, saved.settings.thumbnail_height);
    assert_eq!(85, saved.settings.thumbnail_quality);
    assert_eq!(96, saved.settings.library_batch_size);
    assert_eq!(1, saved.thumbnails_requeued);
    let requeued = repository
        .thumbnail_state(collection_id)
        .expect("requeued thumbnail");
    assert_eq!(ThumbnailStatus::Pending, requeued.status);
    assert_eq!("360x480-q85-webp-v1", requeued.settings_fingerprint);
    assert_eq!(0, requeued.attempts);

    let unchanged = repository
        .save_application_settings(None, 360, 480, 85, "360x480-q85-webp-v1", None, 144)
        .expect("save unchanged thumbnail settings");
    assert_eq!(0, unchanged.thumbnails_requeued);
    assert_eq!(None, unchanged.settings.reader_path);
    assert_eq!(144, unchanged.settings.library_batch_size);
    assert!(matches!(
        repository
            .save_application_settings(None, 300, 400, 0, "invalid", None, 48)
            .expect_err("reject invalid quality"),
        StorageError::InvalidApplicationSettings(_)
    ));
    assert!(matches!(
        repository
            .save_application_settings(None, 360, 480, 85, "360x480-q85-webp-v1", None, 25)
            .expect_err("reject invalid library batch size"),
        StorageError::InvalidApplicationSettings(_)
    ));
    assert_eq!(
        85,
        repository
            .stored_application_settings()
            .expect("settings")
            .expect("row")
            .thumbnail_quality
    );
    assert_eq!(
        144,
        repository
            .stored_application_settings()
            .expect("settings")
            .expect("row")
            .library_batch_size
    );
}

#[test]
fn library_batch_size_defaults_round_trips_reopens_and_falls_back_from_invalid_raw() {
    let tree = TestTree::new("library-batch-size");
    let database = tree.database();
    let repository = CatalogRepository::open(&database).expect("open catalog");
    drop(repository);

    let connection = Connection::open(&database).expect("open raw catalog");
    connection
        .execute(
            "INSERT INTO application_settings(
                 singleton, thumbnail_width, thumbnail_height, thumbnail_quality
             ) VALUES (1, 300, 400, 80)",
            [],
        )
        .expect("insert settings using schema default");
    drop(connection);

    let mut repository = CatalogRepository::open(&database).expect("reopen defaulted catalog");
    assert_eq!(
        48,
        repository
            .stored_application_settings()
            .expect("defaulted settings")
            .expect("settings row")
            .library_batch_size
    );
    for value in [24, 48, 96, 144, 192] {
        let saved = repository
            .save_application_settings(None, 300, 400, 80, "300x400-q80-webp-v1", None, value)
            .expect("save allowed library batch size");
        assert_eq!(value, saved.settings.library_batch_size);
    }
    drop(repository);

    let repository = CatalogRepository::open(&database).expect("reopen saved catalog");
    assert_eq!(
        192,
        repository
            .stored_application_settings()
            .expect("reopened settings")
            .expect("settings row")
            .library_batch_size
    );
    drop(repository);

    let connection = Connection::open(&database).expect("open raw catalog");
    connection
        .pragma_update(None, "ignore_check_constraints", true)
        .expect("allow invalid legacy value");
    connection
        .execute(
            "UPDATE application_settings SET library_batch_size = 25 WHERE singleton = 1",
            [],
        )
        .expect("seed invalid legacy value");
    drop(connection);

    let repository = CatalogRepository::open(&database).expect("reopen invalid catalog");
    assert_eq!(
        48,
        repository
            .stored_application_settings()
            .expect("fallback settings")
            .expect("settings row")
            .library_batch_size
    );
}

#[test]
fn version_one_catalog_upgrades_through_all_migrations_without_losing_data() {
    let tree = TestTree::new("upgrade-v1");
    let database = tree.database();
    let connection = Connection::open(&database).expect("open v1 catalog");
    connection
        .execute_batch(include_str!("../migrations/0001_initial.sql"))
        .expect("apply v1 schema");
    connection
        .execute(
            "INSERT INTO schema_migrations(version, name) VALUES (1, '0001_initial')",
            [],
        )
        .expect("record v1 migration");
    connection
        .execute("INSERT INTO collections DEFAULT VALUES", [])
        .expect("seed v1 data");
    connection
        .pragma_update(None, "user_version", 1)
        .expect("set v1 user version");
    drop(connection);

    let repository = CatalogRepository::open(&database).expect("upgrade catalog");

    assert_eq!(19, repository.schema_version().expect("schema version"));
    assert_eq!(1, repository.collection_count().expect("preserved data"));
    drop(repository);
    let connection = Connection::open(&database).expect("inspect upgraded catalog");
    let guard_exists: bool = connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_schema
                 WHERE type = 'index' AND name = 'scan_runs_single_running'
             )",
            [],
            |row| row.get(0),
        )
        .expect("scan guard index");
    assert!(guard_exists);
}

#[test]
fn version_eight_catalog_removes_is_dl_event_fallback_without_overwriting_manual_event() {
    let tree = TestTree::new("upgrade-v8-revert-dl-event");
    let database = tree.database();
    drop(CatalogRepository::open(&database).expect("create current catalog"));

    let connection = Connection::open(&database).expect("open catalog as v8");
    connection
        .execute_batch(
            "INSERT INTO collections(id) VALUES (1), (2);
             INSERT INTO effective_metadata(collection_id, event, authors_json, is_dl)
             VALUES
                 (1, 'DL', '[]', 1),
                 (2, 'C100', '[]', 1);
             INSERT INTO metadata_assertions(
                 collection_id, field_name, value_json, source_kind, source_reference,
                 confidence_total, status, reason
             ) VALUES
                 (1, 'event', json_quote('DL'), 'inference', 'rule:dl-without-event',
                  1.0, 'accepted', 'dl_without_event_fallback'),
                 (2, 'event', json_quote('DL'), 'inference', 'rule:dl-without-event',
                  1.0, 'accepted', 'dl_without_event_fallback'),
                 (2, 'event', json_quote('C100'), 'manual', 'manual:test',
                  1.0, 'accepted', 'manual test event');
             INSERT INTO metadata_selections(collection_id, field_name, assertion_id, selected_by)
             SELECT 1, 'event', id, 'priority'
             FROM metadata_assertions
             WHERE collection_id = 1 AND source_reference = 'rule:dl-without-event';
             INSERT INTO metadata_selections(collection_id, field_name, assertion_id, selected_by)
             SELECT 2, 'event', id, 'manual'
             FROM metadata_assertions
             WHERE collection_id = 2 AND source_kind = 'manual';
             DROP TABLE external_search_job_resolutions;
             DROP TABLE external_search_batch_items;
             DROP TABLE external_search_batches;
             DROP TABLE vocabulary_exclusions;
             DROP TABLE vocabulary_aliases;
             DROP TABLE saved_views;
             DROP TABLE work_basket_items;
             DROP TABLE cover_selections;
             DROP TABLE work_baskets;
             DROP TABLE duplicate_reviews;
             DROP TABLE duplicate_exclusions;
             DROP TABLE duplicate_scan_items;
             DROP TABLE duplicate_scan_jobs;
             DROP TABLE duplicate_fingerprints;
             DROP TABLE export_job_items;
             DROP TABLE export_jobs;
             DROP TABLE export_roots;
             ALTER TABLE application_settings DROP COLUMN library_batch_size;
             ALTER TABLE application_settings DROP COLUMN default_archive_root_id;
             DELETE FROM schema_migrations WHERE version IN (9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19);
             PRAGMA user_version = 8;",
        )
        .expect("seed v8 metadata");
    drop(connection);

    let repository = CatalogRepository::open(&database).expect("upgrade catalog");
    assert_eq!(19, repository.schema_version().expect("schema version"));
    drop(repository);

    let connection = Connection::open(&database).expect("inspect upgraded catalog");
    let events = (1..=2)
        .map(|collection_id| {
            connection
                .query_row(
                    "SELECT event FROM effective_metadata WHERE collection_id = ?1",
                    [collection_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .expect("read event")
        })
        .collect::<Vec<_>>();
    assert_eq!(vec![None, Some("C100".to_owned())], events);

    let wrong_inference_count = connection
        .query_row(
            "SELECT count(*) FROM metadata_assertions
             WHERE source_reference = 'rule:dl-without-event'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("count wrong inference");
    assert_eq!(0, wrong_inference_count);

    let selected_by = connection
        .query_row(
            "SELECT selected_by FROM metadata_selections
             WHERE collection_id = 2 AND field_name = 'event'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("read preserved selection");
    assert_eq!("manual", selected_by);
}

#[test]
fn version_six_catalog_adds_thumbnail_priority_without_losing_state() {
    let tree = TestTree::new("upgrade-v6-thumbnail-priority");
    let database = tree.database();
    let connection = Connection::open(&database).expect("open v6 catalog");
    for (version, name, sql) in [
        (
            1,
            "0001_initial",
            include_str!("../migrations/0001_initial.sql"),
        ),
        (
            2,
            "0002_scan_run_guard",
            include_str!("../migrations/0002_scan_run_guard.sql"),
        ),
        (
            3,
            "0003_external_search_jobs",
            include_str!("../migrations/0003_external_search_jobs.sql"),
        ),
        (
            4,
            "0004_collection_consolidations",
            include_str!("../migrations/0004_collection_consolidations.sql"),
        ),
        (
            5,
            "0005_thumbnail_states",
            include_str!("../migrations/0005_thumbnail_states.sql"),
        ),
        (
            6,
            "0006_application_settings",
            include_str!("../migrations/0006_application_settings.sql"),
        ),
    ] {
        connection.execute_batch(sql).expect("apply old migration");
        connection
            .execute(
                "INSERT INTO schema_migrations(version, name) VALUES (?1, ?2)",
                rusqlite::params![version, name],
            )
            .expect("record old migration");
    }
    connection
        .execute("INSERT INTO collections DEFAULT VALUES", [])
        .expect("seed collection");
    connection
        .execute(
            "INSERT INTO thumbnail_states(
                 collection_id, source_fingerprint, settings_fingerprint, cache_path, status
             ) VALUES (1, 'source-v1', '300x400-q80', ?1, 'pending')",
            [tree.path.join("1.webp").to_string_lossy().as_ref()],
        )
        .expect("seed thumbnail state");
    connection
        .pragma_update(None, "user_version", 6)
        .expect("set v6 user version");
    drop(connection);

    let repository = CatalogRepository::open(&database).expect("upgrade v6 catalog");
    let state = repository
        .thumbnail_state(1)
        .expect("preserved thumbnail state");

    assert_eq!(19, repository.schema_version().expect("schema version"));
    assert_eq!(ThumbnailStatus::Pending, state.status);
    assert_eq!(BACKGROUND_THUMBNAIL_PRIORITY, state.priority);
    assert!(state.requested_at.is_some());
}

#[test]
fn version_two_catalog_upgrades_external_search_jobs_without_losing_data() {
    let tree = TestTree::new("upgrade-v2-jobs");
    let database = tree.database();
    let connection = Connection::open(&database).expect("open v2 catalog");
    connection
        .execute_batch(include_str!("../migrations/0001_initial.sql"))
        .expect("apply v1 schema");
    connection
        .execute(
            "INSERT INTO schema_migrations(version, name) VALUES (1, '0001_initial')",
            [],
        )
        .expect("record v1 migration");
    connection
        .execute_batch(include_str!("../migrations/0002_scan_run_guard.sql"))
        .expect("apply v2 schema");
    connection
        .execute(
            "INSERT INTO schema_migrations(version, name) VALUES (2, '0002_scan_run_guard')",
            [],
        )
        .expect("record v2 migration");
    connection
        .execute("INSERT INTO collections DEFAULT VALUES", [])
        .expect("seed collection");
    connection
        .execute(
            "INSERT INTO background_jobs(collection_id, job_kind, status, payload_json, attempts)
             VALUES (1, 'external_search', 'pending', '{\"fields\":[\"title\"]}', 2)",
            [],
        )
        .expect("seed external search job");
    let job_id = connection.last_insert_rowid();
    connection
        .pragma_update(None, "user_version", 2)
        .expect("set v2 user version");
    drop(connection);

    let repository = CatalogRepository::open(&database).expect("upgrade v2 catalog");

    assert_eq!(19, repository.schema_version().expect("schema version"));
    let job = repository
        .external_search_job(job_id)
        .expect("preserved external search job");
    assert_eq!(1, job.collection_id);
    assert_eq!(ExternalSearchJobStatus::Pending, job.status);
    assert_eq!(vec![MetadataField::Title], job.fields);
    assert_eq!(2, job.attempts);
}

#[test]
fn version_three_catalog_adds_consolidation_audit_without_losing_data() {
    let tree = TestTree::new("upgrade-v3-consolidation");
    let database = tree.database();
    let connection = Connection::open(&database).expect("open v3 catalog");
    for (version, name, sql) in [
        (
            1,
            "0001_initial",
            include_str!("../migrations/0001_initial.sql"),
        ),
        (
            2,
            "0002_scan_run_guard",
            include_str!("../migrations/0002_scan_run_guard.sql"),
        ),
        (
            3,
            "0003_external_search_jobs",
            include_str!("../migrations/0003_external_search_jobs.sql"),
        ),
    ] {
        connection.execute_batch(sql).expect("apply old migration");
        connection
            .execute(
                "INSERT INTO schema_migrations(version, name) VALUES (?1, ?2)",
                rusqlite::params![version, name],
            )
            .expect("record old migration");
    }
    connection
        .execute("INSERT INTO collections DEFAULT VALUES", [])
        .expect("seed collection");
    connection
        .pragma_update(None, "user_version", 3)
        .expect("set v3 user version");
    drop(connection);

    let repository = CatalogRepository::open(&database).expect("upgrade v3 catalog");

    assert_eq!(19, repository.schema_version().expect("schema version"));
    assert_eq!(1, repository.collection_count().expect("preserved data"));
    assert_eq!(
        None,
        repository
            .merged_into_collection(1)
            .expect("empty consolidation audit")
    );
}

#[test]
fn version_four_catalog_adds_thumbnail_state_without_losing_data() {
    let tree = TestTree::new("upgrade-v4-thumbnails");
    let database = tree.database();
    let connection = Connection::open(&database).expect("open v4 catalog");
    for (version, name, sql) in [
        (
            1,
            "0001_initial",
            include_str!("../migrations/0001_initial.sql"),
        ),
        (
            2,
            "0002_scan_run_guard",
            include_str!("../migrations/0002_scan_run_guard.sql"),
        ),
        (
            3,
            "0003_external_search_jobs",
            include_str!("../migrations/0003_external_search_jobs.sql"),
        ),
        (
            4,
            "0004_collection_consolidations",
            include_str!("../migrations/0004_collection_consolidations.sql"),
        ),
    ] {
        connection.execute_batch(sql).expect("apply old migration");
        connection
            .execute(
                "INSERT INTO schema_migrations(version, name) VALUES (?1, ?2)",
                rusqlite::params![version, name],
            )
            .expect("record old migration");
    }
    connection
        .execute("INSERT INTO collections DEFAULT VALUES", [])
        .expect("seed collection");
    connection
        .pragma_update(None, "user_version", 4)
        .expect("set v4 user version");
    drop(connection);

    let repository = CatalogRepository::open(&database).expect("upgrade v4 catalog");

    assert_eq!(19, repository.schema_version().expect("schema version"));
    assert_eq!(1, repository.collection_count().expect("preserved data"));
    assert!(
        repository
            .table_is_strict("thumbnail_states")
            .expect("STRICT thumbnail states")
    );
}

#[test]
fn version_five_catalog_adds_typed_application_settings_without_losing_data() {
    let tree = TestTree::new("upgrade-v5-settings");
    let database = tree.database();
    let connection = Connection::open(&database).expect("open v5 catalog");
    for (version, name, sql) in [
        (
            1,
            "0001_initial",
            include_str!("../migrations/0001_initial.sql"),
        ),
        (
            2,
            "0002_scan_run_guard",
            include_str!("../migrations/0002_scan_run_guard.sql"),
        ),
        (
            3,
            "0003_external_search_jobs",
            include_str!("../migrations/0003_external_search_jobs.sql"),
        ),
        (
            4,
            "0004_collection_consolidations",
            include_str!("../migrations/0004_collection_consolidations.sql"),
        ),
        (
            5,
            "0005_thumbnail_states",
            include_str!("../migrations/0005_thumbnail_states.sql"),
        ),
    ] {
        connection.execute_batch(sql).expect("apply old migration");
        connection
            .execute(
                "INSERT INTO schema_migrations(version, name) VALUES (?1, ?2)",
                rusqlite::params![version, name],
            )
            .expect("record old migration");
    }
    connection
        .execute("INSERT INTO collections DEFAULT VALUES", [])
        .expect("seed collection");
    connection
        .pragma_update(None, "user_version", 5)
        .expect("set v5 user version");
    drop(connection);

    let repository = CatalogRepository::open(&database).expect("upgrade v5 catalog");

    assert_eq!(19, repository.schema_version().expect("schema version"));
    assert_eq!(1, repository.collection_count().expect("preserved data"));
    assert!(
        repository
            .table_is_strict("application_settings")
            .expect("STRICT application settings")
    );
    assert_eq!(
        None,
        repository
            .stored_application_settings()
            .expect("empty settings")
    );
}

#[test]
fn scanner_result_is_ingested_with_evidence_selection_projection_and_fts() {
    let tree = TestTree::new("ingest");
    let pending = tree.pending("[circle (author、author2)] searchable title.zip");
    let expected_path = pending.path.clone();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");

    let outcome = repository
        .ingest_collection(&pending)
        .expect("ingest collection");

    assert_eq!(IngestOutcome::Inserted, outcome);
    assert_eq!(1, repository.collection_count().expect("collection count"));
    assert_eq!(1, repository.parser_run_count().expect("parser runs"));
    assert_eq!(5, repository.assertion_count().expect("assertions"));
    assert_eq!(
        HashSet::from([expected_path]),
        repository.current_paths().expect("current paths")
    );
    let collection_id = repository
        .first_collection_id()
        .expect("collection id query")
        .expect("collection id");
    assert_eq!(
        Some("\"searchable title\"".to_owned()),
        repository
            .current_value_json(collection_id, "title")
            .expect("current title")
    );
    assert_eq!(
        vec!["searchable title".to_owned()],
        repository.search_titles("searchable").expect("FTS search")
    );
}

#[test]
fn latest_parser_identifiers_preserve_typed_rj_evidence() {
    let tree = TestTree::new("parser-identifiers");
    let pending = tree.pending("[rj407766] [Circle] identified title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection id query")
        .expect("collection id");

    let identifiers = repository
        .latest_parser_identifiers(collection_id)
        .expect("latest identifiers");

    assert_eq!(1, identifiers.len());
    assert_eq!("RJ", identifiers[0].scheme);
    assert_eq!("RJ407766", identifiers[0].value);
    assert_eq!("[rj407766]", identifiers[0].raw);
}

#[test]
fn active_collections_support_safe_search_paging_and_detail() {
    let tree = TestTree::new("collection-query");
    let first = tree.pending("[AlphaCircle (Alice)] searchable first.zip");
    let second = tree.pending("[BetaCircle (Bob)] second title.zip");
    let third = tree.pending("RJ123456 [GammaCircle] filename marker.zip");
    let first_path = first.path.clone();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.ingest_collection(&first).expect("ingest first");
    repository
        .ingest_collection(&second)
        .expect("ingest second");
    repository.ingest_collection(&third).expect("ingest third");
    let first_id = repository
        .collection_id_for_current_path(&first_path)
        .expect("first lookup")
        .expect("first collection");
    std::thread::sleep(Duration::from_millis(2));
    repository
        .set_manual_value(
            first_id,
            MetadataField::Parody,
            MetadataValue::Parody(Parody {
                raw: "Fate raw".to_owned(),
                canonical: "Fate canonical".to_owned(),
                evidence: "manual test".to_owned(),
            }),
        )
        .expect("set parody");

    let first_page = repository
        .collections(&CollectionQuery {
            search: None,
            page: 1,
            per_page: 2,
            ..CollectionQuery::default()
        })
        .expect("first page");
    assert_eq!(3, first_page.total);
    assert_eq!(2, first_page.items.len());
    assert_eq!(2, first_page.per_page);
    assert!(first_page.items[0].id > first_page.items[1].id);

    let second_page = repository
        .collections(&CollectionQuery {
            search: None,
            page: 2,
            per_page: 2,
            ..CollectionQuery::default()
        })
        .expect("second page");
    assert_eq!(1, second_page.items.len());
    assert_eq!(first_id, second_page.items[0].id);

    let title_ascending = repository
        .collections(&CollectionQuery {
            sort: CollectionSort::Title,
            direction: SortDirection::Ascending,
            ..CollectionQuery::default()
        })
        .expect("title ascending");
    assert_eq!(
        vec![
            "RJ123456 [GammaCircle] filename marker",
            "searchable first",
            "second title"
        ],
        title_ascending
            .items
            .iter()
            .map(|item| item.title.as_deref().expect("title"))
            .collect::<Vec<_>>()
    );
    let title_descending = repository
        .collections(&CollectionQuery {
            sort: CollectionSort::Title,
            direction: SortDirection::Descending,
            ..CollectionQuery::default()
        })
        .expect("title descending");
    assert_eq!(
        vec![
            "second title",
            "searchable first",
            "RJ123456 [GammaCircle] filename marker"
        ],
        title_descending
            .items
            .iter()
            .map(|item| item.title.as_deref().expect("title"))
            .collect::<Vec<_>>()
    );
    let recently_updated = repository
        .collections(&CollectionQuery {
            sort: CollectionSort::Updated,
            direction: SortDirection::Descending,
            ..CollectionQuery::default()
        })
        .expect("recently updated");
    assert_eq!(first_id, recently_updated.items[0].id);

    let title_located = repository
        .locate_collection(
            first_id,
            &CollectionQuery {
                sort: CollectionSort::Title,
                direction: SortDirection::Ascending,
                per_page: 2,
                ..CollectionQuery::default()
            },
        )
        .expect("locate in title ordering");
    assert_eq!(Some(2), title_located.position);
    assert_eq!(Some(1), title_located.page);

    let located = repository
        .locate_collection(
            first_id,
            &CollectionQuery {
                per_page: 2,
                ..CollectionQuery::default()
            },
        )
        .expect("locate collection in query");
    assert_eq!(Some(3), located.position);
    assert_eq!(Some(2), located.page);
    assert_eq!(first_id, located.collection.id);

    let outside_query = repository
        .locate_collection(
            first_id,
            &CollectionQuery {
                search: Some("BetaCircle".to_owned()),
                per_page: 2,
                ..CollectionQuery::default()
            },
        )
        .expect("locate collection outside query");
    assert_eq!(None, outside_query.position);
    assert_eq!(None, outside_query.page);

    for term in ["searchable", "AlphaCircle", "Alice", "Fate"] {
        let metadata_match = repository
            .collections(&CollectionQuery {
                search: Some(term.to_owned()),
                ..CollectionQuery::default()
            })
            .expect("metadata search");
        assert_eq!(vec![first_id], collection_ids(&metadata_match.items));
    }

    let filename_match = repository
        .collections(&CollectionQuery {
            search: Some("RJ123456".to_owned()),
            ..CollectionQuery::default()
        })
        .expect("filename search");
    assert_eq!(1, filename_match.items.len());
    assert_eq!(
        "RJ123456 [GammaCircle] filename marker.zip",
        filename_match.items[0].filename
    );

    let quote_only = repository
        .collections(&CollectionQuery {
            search: Some("\"".to_owned()),
            page: 0,
            per_page: 500,
            ..CollectionQuery::default()
        })
        .expect("quote-only search");
    assert_eq!(3, quote_only.total);
    assert_eq!(1, quote_only.page);
    assert_eq!(200, quote_only.per_page);

    let syntax_like_input = repository
        .collections(&CollectionQuery {
            search: Some("\" OR 1=1 --\0".to_owned()),
            ..CollectionQuery::default()
        })
        .expect("syntax-like search is data, not FTS syntax");
    assert_eq!(0, syntax_like_input.total);

    let detail = repository.collection(first_id).expect("collection detail");
    assert_eq!(first_path, detail.path);
    assert_eq!(Some("searchable first".to_owned()), detail.title);
    assert_eq!(Some("AlphaCircle".to_owned()), detail.circle);
    assert_eq!(vec!["Alice".to_owned()], detail.authors);
    assert_eq!(Some("Fate canonical".to_owned()), detail.parody);
    assert_eq!(Some("Fate raw".to_owned()), detail.parody_raw);
    assert!(detail.tags.is_empty());
    assert!(matches!(
        detail.root,
        Some(CollectionRootSnapshot {
            source: SourceKind::Archive,
            ..
        })
    ));

    fs::remove_file(&first_path).expect("remove first collection");
    repository
        .mark_collection_missing(first_id)
        .expect("mark first missing");
    assert_eq!(
        2,
        repository
            .collections(&CollectionQuery::default())
            .expect("active collections")
            .total
    );
    assert!(matches!(
        repository.collection(first_id),
        Err(StorageError::CollectionNotFound(id)) if id == first_id
    ));
}

#[test]
fn collection_statistics_count_only_active_library_values_and_split_authors() {
    let tree = TestTree::new("collection-statistics");
    let first = tree.pending("[CircleA (Shared)] first.zip");
    let second = tree.pending("[CircleA (Shared、Other)] second.zip");
    let third = tree.pending("[CircleB (Third)] third.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    for pending in [&first, &second, &third] {
        repository
            .ingest_collection(pending)
            .expect("ingest collection");
    }
    let id = |pending: &PendingCollection| {
        repository
            .collection_id_for_current_path(&pending.path)
            .expect("collection lookup")
            .expect("collection ID")
    };
    let first_id = id(&first);
    let second_id = id(&second);
    let third_id = id(&third);
    for collection_id in [first_id, second_id] {
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Event,
                MetadataValue::Text("C100".to_owned()),
            )
            .expect("set event");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Circle,
                MetadataValue::Text("CircleA".to_owned()),
            )
            .expect("set circle");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Parody,
                MetadataValue::Parody(Parody {
                    raw: "Work".to_owned(),
                    canonical: "Work".to_owned(),
                    evidence: "manual test".to_owned(),
                }),
            )
            .expect("set parody");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Classification,
                MetadataValue::Classification(Classification {
                    top_level: "同人誌".to_owned(),
                    subcategory: None,
                    raw_marker: None,
                }),
            )
            .expect("set classification");
    }
    repository
        .set_manual_value(
            first_id,
            MetadataField::Authors,
            MetadataValue::Authors(Authors {
                raw: Some("Shared".to_owned()),
                values: vec!["Shared".to_owned()],
            }),
        )
        .expect("set first authors");
    repository
        .set_manual_value(
            second_id,
            MetadataField::Authors,
            MetadataValue::Authors(Authors {
                raw: Some("Shared、Other".to_owned()),
                values: vec!["Shared".to_owned(), "Other".to_owned()],
            }),
        )
        .expect("set second authors");
    repository
        .set_manual_value(
            third_id,
            MetadataField::Classification,
            MetadataValue::Classification(Classification {
                top_level: "商業誌".to_owned(),
                subcategory: Some("雜誌".to_owned()),
                raw_marker: None,
            }),
        )
        .expect("set commercial classification");
    repository
        .add_collection_tag(first_id, "favorite")
        .expect("tag first");
    repository
        .add_collection_tag(second_id, "favorite")
        .expect("tag second");

    let statistics = repository.collection_statistics().expect("statistics");

    assert_eq!(3, statistics.total);
    assert_eq!(2, statistics.tagged);
    assert_eq!(1, statistics.missing_metadata);
    assert_eq!("同人誌", statistics.classifications[0].name);
    assert_eq!(2, statistics.classifications[0].count);
    assert_eq!("商業誌", statistics.classifications[1].name);
    assert_eq!(1, statistics.classifications[1].count);
    assert_eq!("Shared", statistics.top_authors[0].name);
    assert_eq!(2, statistics.top_authors[0].count);
    assert_eq!("Work", statistics.top_parodies[0].name);
    assert_eq!(2, statistics.top_parodies[0].count);
    assert_eq!("CircleA", statistics.top_circles[0].name);
    assert_eq!(2, statistics.top_circles[0].count);
    assert_eq!("C100", statistics.top_events[0].name);
    assert_eq!(2, statistics.top_events[0].count);
    assert_eq!("favorite", statistics.top_tags[0].name);
    assert_eq!(2, statistics.top_tags[0].count);

    let authors = repository
        .collection_facets(CollectionFacet::Author, "har", 20)
        .expect("author facets");
    assert_eq!("Shared", authors[0].name);
    assert_eq!(2, authors[0].count);
    let tags = repository
        .collection_facets(CollectionFacet::Tag, "fav", 20)
        .expect("tag facets");
    assert_eq!("favorite", tags[0].name);
    assert_eq!(2, tags[0].count);
    assert!(
        repository
            .collection_facets(CollectionFacet::Event, "%", 20)
            .expect("escaped facet search")
            .is_empty()
    );
}

#[test]
fn collection_facets_merge_ascii_case_variants_like_collection_filters() {
    let tree = TestTree::new("collection-facet-case");
    let lowercase = tree.pending("[SeedCircle (SeedAuthor)] lowercase.zip");
    let uppercase = tree.pending("[SeedCircle (SeedAuthor)] uppercase.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    for pending in [&lowercase, &uppercase] {
        repository
            .ingest_collection(pending)
            .expect("ingest collection");
    }
    let ids = [&lowercase, &uppercase].map(|pending| {
        repository
            .collection_id_for_current_path(&pending.path)
            .expect("collection lookup")
            .expect("collection ID")
    });

    for (collection_id, value) in [(ids[0], "foo"), (ids[1], "Foo")] {
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Event,
                MetadataValue::Text(value.to_owned()),
            )
            .expect("set event");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Circle,
                MetadataValue::Text(value.to_owned()),
            )
            .expect("set circle");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Authors,
                MetadataValue::Authors(Authors {
                    raw: Some(value.to_owned()),
                    values: vec![value.to_owned()],
                }),
            )
            .expect("set author");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Parody,
                MetadataValue::Parody(Parody {
                    raw: value.to_owned(),
                    canonical: value.to_owned(),
                    evidence: "manual test".to_owned(),
                }),
            )
            .expect("set parody");
        repository
            .add_collection_tag(collection_id, value)
            .expect("add tag");
    }

    let cases = [
        (
            CollectionFacet::Event,
            CollectionFilters {
                event: Some("Foo".to_owned()),
                ..CollectionFilters::default()
            },
        ),
        (
            CollectionFacet::Circle,
            CollectionFilters {
                circle: Some("Foo".to_owned()),
                ..CollectionFilters::default()
            },
        ),
        (
            CollectionFacet::Author,
            CollectionFilters {
                author: Some("Foo".to_owned()),
                ..CollectionFilters::default()
            },
        ),
        (
            CollectionFacet::Parody,
            CollectionFilters {
                parody: Some("Foo".to_owned()),
                ..CollectionFilters::default()
            },
        ),
        (
            CollectionFacet::Tag,
            CollectionFilters {
                tags: vec!["Foo".to_owned()],
                ..CollectionFilters::default()
            },
        ),
    ];

    for (facet, filters) in cases {
        let facets = repository
            .collection_facets(facet, "foo", 20)
            .expect("case-insensitive facets");
        assert_eq!(1, facets.len(), "{facet:?}");
        assert_eq!("Foo", facets[0].name, "{facet:?}");

        let filtered = repository
            .collections(&CollectionQuery {
                filters,
                ..CollectionQuery::default()
            })
            .expect("case-insensitive collection filter");
        assert_eq!(filtered.total, facets[0].count, "{facet:?}");
        assert_eq!(2, filtered.total, "{facet:?}");
    }
}

fn collection_ids(collections: &[doujin_storage::collections::CollectionSnapshot]) -> Vec<i64> {
    collections.iter().map(|collection| collection.id).collect()
}

#[test]
fn collection_filters_combine_metadata_missing_source_and_all_tags() {
    let tree = TestTree::new("collection-filters");
    let first = tree.pending("[AlphaCircle (Alice)] first.zip");
    let second = tree.pending("[AlphaCircle (Bob)] second.zip");
    let third = tree.pending_under(
        "downloads",
        SourceKind::Downloads,
        "[AlphaCircle (Alice)] third.zip",
    );
    let fourth = tree.pending("[NoAuthorCircle] fourth.zip");
    let paths = [
        first.path.clone(),
        second.path.clone(),
        third.path.clone(),
        fourth.path.clone(),
    ];
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    for pending in [&first, &second, &third, &fourth] {
        repository
            .ingest_collection(pending)
            .expect("ingest filtered collection");
    }
    let ids = paths.map(|path| {
        repository
            .collection_id_for_current_path(&path)
            .expect("collection lookup")
            .expect("collection ID")
    });

    for collection_id in [ids[0], ids[1]] {
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Event,
                MetadataValue::Text("C106".to_owned()),
            )
            .expect("set C106");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Parody,
                MetadataValue::Parody(Parody {
                    raw: "Fate raw".to_owned(),
                    canonical: "Fate".to_owned(),
                    evidence: "manual test".to_owned(),
                }),
            )
            .expect("set parody");
    }
    repository
        .set_manual_value(
            ids[2],
            MetadataField::Event,
            MetadataValue::Text("C107".to_owned()),
        )
        .expect("set C107");
    repository
        .set_manual_value(
            ids[2],
            MetadataField::Parody,
            MetadataValue::Parody(Parody {
                raw: "Fate raw".to_owned(),
                canonical: "Fate".to_owned(),
                evidence: "manual test".to_owned(),
            }),
        )
        .expect("set parody");
    for (collection_id, subcategory) in [(ids[0], "漫畫"), (ids[1], "小說"), (ids[2], "漫畫")]
    {
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Classification,
                MetadataValue::Classification(Classification {
                    top_level: "商業誌".to_owned(),
                    subcategory: Some(subcategory.to_owned()),
                    raw_marker: None,
                }),
            )
            .expect("set classification");
    }
    for (collection_id, tags) in [
        (ids[0], &["favorite", "color"][..]),
        (ids[1], &["favorite"][..]),
        (ids[2], &["favorite", "color"][..]),
    ] {
        for tag in tags {
            repository
                .add_collection_tag(collection_id, tag)
                .expect("add tag");
        }
    }

    let combined = repository
        .collections(&CollectionQuery {
            filters: CollectionFilters {
                event: Some("C106".to_owned()),
                circle: Some("AlphaCircle".to_owned()),
                author: Some("Alice".to_owned()),
                parody: Some("Fate".to_owned()),
                classification: Some("商業誌".to_owned()),
                subcategory: Some("漫畫".to_owned()),
                source: Some(SourceKind::Archive),
                tags: vec!["favorite".to_owned(), "color".to_owned()],
                untagged: false,
                missing: Vec::new(),
            },
            ..CollectionQuery::default()
        })
        .expect("combined filters");
    assert_eq!(vec![ids[0]], collection_ids(&combined.items));

    let all_tags = repository
        .collections(&CollectionQuery {
            filters: CollectionFilters {
                tags: vec!["favorite".to_owned(), "color".to_owned()],
                ..CollectionFilters::default()
            },
            ..CollectionQuery::default()
        })
        .expect("all tags");
    assert_eq!(
        HashSet::from([ids[0], ids[2]]),
        collection_ids(&all_tags.items).into_iter().collect()
    );

    let missing = repository
        .collections(&CollectionQuery {
            filters: CollectionFilters {
                missing: vec![MissingMetadataField::Event, MissingMetadataField::Authors],
                ..CollectionFilters::default()
            },
            ..CollectionQuery::default()
        })
        .expect("missing metadata");
    assert_eq!(vec![ids[3]], collection_ids(&missing.items));

    let missing_any = repository
        .collections(&CollectionQuery {
            filters: CollectionFilters {
                missing: vec![MissingMetadataField::Any],
                ..CollectionFilters::default()
            },
            ..CollectionQuery::default()
        })
        .expect("missing any metadata");
    assert_eq!(vec![ids[3]], collection_ids(&missing_any.items));

    let untagged = repository
        .collections(&CollectionQuery {
            filters: CollectionFilters {
                untagged: true,
                ..CollectionFilters::default()
            },
            ..CollectionQuery::default()
        })
        .expect("untagged collections");
    assert_eq!(vec![ids[3]], collection_ids(&untagged.items));

    let parameterized = repository
        .collections(&CollectionQuery {
            filters: CollectionFilters {
                circle: Some("AlphaCircle' OR 1=1 --".to_owned()),
                ..CollectionFilters::default()
            },
            ..CollectionQuery::default()
        })
        .expect("parameterized filter");
    assert_eq!(0, parameterized.total);
    assert_eq!(
        vec!["color".to_owned(), "favorite".to_owned()],
        repository.collection(ids[0]).expect("tagged detail").tags
    );
}

#[test]
fn review_queue_queries_missing_and_candidate_items_with_server_side_totals() {
    let tree = TestTree::new("review-queue");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut ids = Vec::new();
    for filename in [
        "[Missing] missing.zip",
        "[Candidate] candidate.zip",
        "[Clean] clean.zip",
    ] {
        repository
            .ingest_collection(&tree.pending(filename))
            .expect("ingest review fixture");
        ids.push(
            repository
                .collection_id_for_current_path(&tree.path.join(filename))
                .expect("collection lookup")
                .expect("collection ID"),
        );
    }
    let missing_id = ids[0];
    let candidate_id = ids[1];
    let clean_id = ids[2];

    for collection_id in [candidate_id, clean_id] {
        for (field, value) in [
            (
                MetadataField::Title,
                MetadataValue::Text(format!("review title {collection_id}")),
            ),
            (MetadataField::Event, MetadataValue::Text("C106".to_owned())),
            (
                MetadataField::Circle,
                MetadataValue::Text("review circle".to_owned()),
            ),
            (
                MetadataField::Authors,
                MetadataValue::Authors(Authors {
                    raw: Some("review author".to_owned()),
                    values: vec!["review author".to_owned()],
                }),
            ),
            (
                MetadataField::Parody,
                MetadataValue::Parody(Parody {
                    raw: "review parody".to_owned(),
                    canonical: "review parody".to_owned(),
                    evidence: "manual review fixture".to_owned(),
                }),
            ),
            (
                MetadataField::Classification,
                MetadataValue::Classification(Classification {
                    top_level: "同人誌".to_owned(),
                    subcategory: None,
                    raw_marker: None,
                }),
            ),
        ] {
            repository
                .set_manual_value(collection_id, field, value)
                .expect("complete primary metadata");
        }
    }
    let suggestion = repository
        .save_external_candidate(ExternalCandidate {
            collection_id: candidate_id,
            field: MetadataField::Title,
            value: MetadataValue::Text("candidate title".to_owned()),
            source_reference: "provider:review-candidate".to_owned(),
            confidence: confidence(0.8, false),
        })
        .expect("save review candidate");
    let ExternalCandidateOutcome::Suggestion { assertion_id, .. } = suggestion else {
        panic!("review fixture must create a candidate assertion");
    };

    let all = repository
        .review_queue(&ReviewQueueQuery {
            per_page: 1,
            ..ReviewQueueQuery::default()
        })
        .expect("all review items");
    assert_eq!(2, all.total);
    assert_eq!(1, all.items.len());
    assert_eq!(candidate_id, all.items[0].collection.id);
    assert!(
        all.items[0]
            .metadata
            .fields
            .iter()
            .flat_map(|field| &field.assertions)
            .any(|assertion| assertion.id == assertion_id
                && assertion.status == MetadataAssertionStatus::Candidate)
    );

    let missing = repository
        .review_queue(&ReviewQueueQuery {
            kind: ReviewQueueKind::Missing,
            ..ReviewQueueQuery::default()
        })
        .expect("missing review items");
    assert_eq!(1, missing.total);
    assert_eq!(missing_id, missing.items[0].collection.id);

    let candidate = repository
        .review_queue(&ReviewQueueQuery {
            kind: ReviewQueueKind::Candidate,
            ..ReviewQueueQuery::default()
        })
        .expect("candidate review items");
    assert_eq!(1, candidate.total);
    assert_eq!(candidate_id, candidate.items[0].collection.id);

    repository
        .decide_metadata_assertion(
            candidate_id,
            MetadataField::Title,
            assertion_id,
            MetadataAssertionDecision::Reject,
        )
        .expect("reject review candidate through existing contract");
    assert_eq!(
        0,
        repository
            .review_queue(&ReviewQueueQuery {
                kind: ReviewQueueKind::Candidate,
                ..ReviewQueueQuery::default()
            })
            .expect("candidate leaves queue")
            .total
    );

    for (field, value) in [
        (MetadataField::Event, MetadataValue::Text("C106".to_owned())),
        (
            MetadataField::Authors,
            MetadataValue::Authors(Authors {
                raw: Some("manual author".to_owned()),
                values: vec!["manual author".to_owned()],
            }),
        ),
        (
            MetadataField::Parody,
            MetadataValue::Parody(Parody {
                raw: "manual parody".to_owned(),
                canonical: "manual parody".to_owned(),
                evidence: "manual review fixture".to_owned(),
            }),
        ),
        (
            MetadataField::Classification,
            MetadataValue::Classification(Classification {
                top_level: "同人誌".to_owned(),
                subcategory: None,
                raw_marker: None,
            }),
        ),
    ] {
        repository
            .set_manual_value(missing_id, field, value)
            .expect("complete missing review metadata");
    }
    assert_eq!(
        0,
        repository
            .review_queue(&ReviewQueueQuery {
                kind: ReviewQueueKind::Missing,
                ..ReviewQueueQuery::default()
            })
            .expect("completed metadata leaves queue")
            .total
    );
}

#[test]
fn collection_tags_are_idempotent_and_remove_orphans() {
    let tree = TestTree::new("tag-lifecycle");
    let first = tree.pending("[circle] first.zip");
    let second = tree.pending("[circle] second.zip");
    let first_path = first.path.clone();
    let second_path = second.path.clone();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.ingest_collection(&first).expect("ingest first");
    repository
        .ingest_collection(&second)
        .expect("ingest second");
    let first_id = repository
        .collection_id_for_current_path(&first_path)
        .expect("first lookup")
        .expect("first ID");
    let second_id = repository
        .collection_id_for_current_path(&second_path)
        .expect("second lookup")
        .expect("second ID");

    let tag_id = repository
        .add_collection_tag(first_id, "  favorite  ")
        .expect("add tag");
    assert_eq!(
        tag_id,
        repository
            .add_collection_tag(first_id, "favorite")
            .expect("repeat tag")
    );
    assert_eq!(1, repository.tag_count().expect("tag count"));
    assert_eq!(
        vec!["favorite".to_owned()],
        repository.collection(first_id).expect("first detail").tags
    );

    repository
        .add_collection_tag(second_id, "favorite")
        .expect("share tag");
    assert!(
        repository
            .remove_collection_tag(first_id, "favorite")
            .expect("remove first link")
    );
    assert_eq!(1, repository.tag_count().expect("shared tag remains"));
    assert!(
        !repository
            .remove_collection_tag(first_id, "favorite")
            .expect("repeated remove")
    );
    assert!(
        repository
            .remove_collection_tag(second_id, "favorite")
            .expect("remove final link")
    );
    assert_eq!(0, repository.tag_count().expect("orphan removed"));
    assert!(matches!(
        repository.add_collection_tag(first_id, "   "),
        Err(StorageError::InvalidMetadata(_))
    ));
}

#[test]
fn external_tags_require_suggestion_confidence_and_are_idempotent() {
    let tree = TestTree::new("external-tag-lifecycle");
    let pending = tree.pending("[circle] tagged.zip");
    let path = pending.path.clone();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .collection_id_for_current_path(&path)
        .expect("collection lookup")
        .expect("collection ID");
    let tag = || ExternalTag {
        collection_id,
        name: "female:big breasts".to_owned(),
        source_reference: "https://e-hentai.org/g/1/0123456789/".to_owned(),
        confidence: confidence(0.88, false),
    };

    assert!(matches!(
        repository
            .save_external_tag(tag())
            .expect("add external tag"),
        ExternalTagOutcome::Applied { .. }
    ));
    assert!(matches!(
        repository
            .save_external_tag(tag())
            .expect("repeat external tag"),
        ExternalTagOutcome::Existing { .. }
    ));
    assert_eq!(
        vec!["female:big breasts".to_owned()],
        repository
            .collection(collection_id)
            .expect("tagged collection")
            .tags
    );

    let mut low_confidence = tag();
    low_confidence.name = "female:milf".to_owned();
    low_confidence.confidence = confidence(0.74, false);
    assert!(matches!(
        repository.save_external_tag(low_confidence),
        Err(StorageError::InvalidMetadata(_))
    ));
}

#[test]
fn duplicate_current_path_is_explicitly_skipped() {
    let tree = TestTree::new("duplicate");
    let pending = tree.pending("[circle] title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");

    repository
        .ingest_collection(&pending)
        .expect("first ingest");
    let second = repository
        .ingest_collection(&pending)
        .expect("second ingest");

    assert_eq!(IngestOutcome::SkippedExisting, second);
    assert_eq!(1, repository.collection_count().expect("collection count"));
}

#[test]
fn invalid_projection_rolls_back_the_entire_collection() {
    let tree = TestTree::new("rollback");
    let mut invalid = tree.pending("[circle] invalid title.zip");
    invalid.parsed.title = " ".to_owned();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");

    let error = repository
        .ingest_collection(&invalid)
        .expect_err("constraint failure");

    assert!(matches!(error, StorageError::Ingest { path, .. } if path == invalid.path));
    assert_eq!(0, repository.collection_count().expect("collection count"));
    assert_eq!(0, repository.library_root_count().expect("root count"));
    assert_eq!(0, repository.parser_run_count().expect("parser count"));
    assert_eq!(0, repository.assertion_count().expect("assertion count"));
    assert_eq!(
        0,
        repository
            .effective_metadata_count()
            .expect("projection count")
    );
}

#[test]
fn parser_history_keeps_the_pre_decode_filename() {
    let tree = TestTree::new("raw-filename");
    let mut pending = tree.pending("(C77) [circle] title.zip");
    let original = tree.path.join("%28C77%29%20%5Bcircle%5D%20title.zip");
    pending.filename_normalization = FilenameNormalization::Renamed {
        original,
        renamed: pending.path.clone(),
    };
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");

    repository
        .ingest_collection(&pending)
        .expect("ingest renamed collection");

    assert_eq!(
        Some("%28C77%29%20%5Bcircle%5D%20title.zip".to_owned()),
        repository
            .first_parser_raw_filename()
            .expect("raw filename")
    );
}

#[test]
fn file_catalog_uses_wal_and_rejects_a_future_schema() {
    let tree = TestTree::new("file-catalog");
    let database = tree.database();
    {
        let repository = CatalogRepository::open(&database).expect("open file catalog");
        assert_eq!("wal", repository.journal_mode().expect("journal mode"));
    }

    let future = tree.path.join("future.db");
    let connection = Connection::open(&future).expect("open future catalog");
    connection
        .pragma_update(None, "user_version", 99)
        .expect("set future schema");
    drop(connection);

    assert!(matches!(
        CatalogRepository::open(&future),
        Err(StorageError::UnsupportedSchemaVersion(99))
    ));
}

#[test]
fn unversioned_non_empty_database_is_never_upgraded_in_place() {
    let tree = TestTree::new("legacy-guard");
    let legacy = tree.path.join("legacy.db");
    let connection = Connection::open(&legacy).expect("open legacy catalog");
    connection
        .execute(
            "CREATE TABLE legacy_collections(id INTEGER PRIMARY KEY)",
            [],
        )
        .expect("create legacy table");
    drop(connection);

    assert!(matches!(
        CatalogRepository::open(&legacy),
        Err(StorageError::UnversionedNonEmptyCatalog)
    ));

    let connection = Connection::open(&legacy).expect("reopen legacy catalog");
    let table_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema
             WHERE type = 'table' AND name = 'legacy_collections'",
            [],
            |row| row.get(0),
        )
        .expect("legacy table remains");
    let v2_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema
             WHERE type = 'table' AND name = 'collections'",
            [],
            |row| row.get(0),
        )
        .expect("v2 table absent");
    assert_eq!(1, table_count);
    assert_eq!(0, v2_count);
}

#[test]
fn path_keys_are_case_and_separator_insensitive() {
    assert_eq!(
        path_key(Path::new(r"D:\Library\Circle\Title.zip")),
        path_key(Path::new("d:/library/circle/title.zip"))
    );
}

#[test]
fn manual_value_wins_and_clear_restores_the_filename_candidate() {
    let tree = TestTree::new("manual-clear");
    let pending = tree.pending("[circle] filename title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    let before = repository.assertion_count().expect("assertion count");

    repository
        .set_manual_value(
            collection_id,
            MetadataField::Title,
            MetadataValue::Text("manual title".to_owned()),
        )
        .expect("set manual title");

    let selection = repository
        .current_selection(collection_id, MetadataField::Title)
        .expect("selection")
        .expect("selected title");
    assert_eq!(MetadataSource::Manual, selection.source);
    assert!(selection.selected_manually);
    assert_eq!("\"manual title\"", selection.value_json);
    assert_eq!(
        vec!["manual title".to_owned()],
        repository.search_titles("manual").expect("manual search")
    );
    assert!(
        repository
            .search_titles("filename")
            .expect("old title search")
            .is_empty()
    );

    assert!(
        repository
            .clear_manual_value(collection_id, MetadataField::Title)
            .expect("clear manual title")
    );
    let restored = repository
        .current_selection(collection_id, MetadataField::Title)
        .expect("restored selection")
        .expect("restored title");
    assert_eq!(MetadataSource::Filename, restored.source);
    assert!(!restored.selected_manually);
    assert_eq!("\"filename title\"", restored.value_json);
    assert_eq!(
        before + 1,
        repository.assertion_count().expect("history kept")
    );
    assert!(
        !repository
            .clear_manual_value(collection_id, MetadataField::Title)
            .expect("second clear is a no-op")
    );
}

#[test]
fn clearing_the_only_manual_candidate_leaves_the_field_unselected() {
    let tree = TestTree::new("manual-only");
    let pending = tree.pending("[circle] title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");

    repository
        .set_manual_value(
            collection_id,
            MetadataField::Event,
            MetadataValue::Text("manual event".to_owned()),
        )
        .expect("set event");
    assert!(
        repository
            .clear_manual_value(collection_id, MetadataField::Event)
            .expect("clear event")
    );

    assert!(
        repository
            .current_selection(collection_id, MetadataField::Event)
            .expect("event selection")
            .is_none()
    );
}

#[test]
fn source_priority_and_external_confidence_rules_are_enforced() {
    let tree = TestTree::new("confidence");
    let pending = tree.pending("(C77) [circle] filename title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    let assertions_before = repository.assertion_count().expect("assertion count");

    let low = repository
        .save_external_candidate(ExternalCandidate {
            collection_id,
            field: MetadataField::Title,
            value: MetadataValue::Text("low confidence".to_owned()),
            source_reference: "provider:item-low".to_owned(),
            confidence: confidence(0.74, false),
        })
        .expect("save low confidence result");
    assert!(matches!(low, ExternalCandidateOutcome::SearchOnly { .. }));
    assert_eq!(
        assertions_before,
        repository.assertion_count().expect("no low assertion")
    );

    let medium = repository
        .save_external_candidate(ExternalCandidate {
            collection_id,
            field: MetadataField::Title,
            value: MetadataValue::Text("external suggestion".to_owned()),
            source_reference: "provider:item-medium".to_owned(),
            confidence: confidence(0.8, false),
        })
        .expect("save medium confidence candidate");
    let ExternalCandidateOutcome::Suggestion { assertion_id, .. } = medium else {
        panic!("medium result must await confirmation");
    };
    assert_eq!(
        MetadataSource::Filename,
        repository
            .current_selection(collection_id, MetadataField::Title)
            .expect("title selection")
            .expect("title")
            .source
    );

    repository
        .decide_metadata_assertion(
            collection_id,
            MetadataField::Title,
            assertion_id,
            MetadataAssertionDecision::Select,
        )
        .expect("confirm external candidate");
    let confirmed = repository
        .current_selection(collection_id, MetadataField::Title)
        .expect("confirmed selection")
        .expect("confirmed title");
    assert_eq!(MetadataSource::External, confirmed.source);
    assert!(confirmed.selected_manually);

    let automatic = repository
        .save_external_candidate(ExternalCandidate {
            collection_id,
            field: MetadataField::Event,
            value: MetadataValue::Text("C106".to_owned()),
            source_reference: "provider:item-high".to_owned(),
            confidence: confidence(0.95, true),
        })
        .expect("save high confidence candidate");
    assert!(matches!(
        automatic,
        ExternalCandidateOutcome::AutoApplied { .. }
    ));
    let event = repository
        .current_selection(collection_id, MetadataField::Event)
        .expect("event selection")
        .expect("event");
    assert_eq!(MetadataSource::External, event.source);
    assert!(!event.selected_manually);
    assert_eq!("\"C106\"", event.value_json);
    assert_eq!(
        3,
        repository
            .external_search_result_count()
            .expect("search result count")
    );
}

#[test]
fn identical_external_candidates_reuse_assertion_and_preserve_decisions() {
    let tree = TestTree::new("external-candidate-idempotency");
    let pending = tree.pending("[circle] filename title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    let assertions_before = repository.assertion_count().expect("assertion count");
    let candidate = || ExternalCandidate {
        collection_id,
        field: MetadataField::Parody,
        value: MetadataValue::Parody(Parody {
            raw: "オリジナル作品".to_owned(),
            canonical: "オリジナル".to_owned(),
            evidence: "dlsite_exact_title:work_options:ORW".to_owned(),
        }),
        source_reference: "https://www.dlsite.com/maniax/work/=/product_id/RJ338758.html"
            .to_owned(),
        confidence: confidence(0.85, false),
    };

    let first = repository
        .save_external_candidate(candidate())
        .expect("save first suggestion");
    let ExternalCandidateOutcome::Suggestion {
        search_result_id: first_result_id,
        assertion_id,
    } = first
    else {
        panic!("first candidate must be a suggestion");
    };
    let repeated = repository
        .save_external_candidate(candidate())
        .expect("save repeated suggestion");
    let ExternalCandidateOutcome::Suggestion {
        search_result_id: repeated_result_id,
        assertion_id: repeated_assertion_id,
    } = repeated
    else {
        panic!("repeated candidate must reuse a suggestion");
    };
    assert_ne!(first_result_id, repeated_result_id);
    assert_eq!(assertion_id, repeated_assertion_id);
    assert_eq!(
        assertions_before + 1,
        repository.assertion_count().expect("one new assertion")
    );
    assert_eq!(
        2,
        repository
            .external_search_result_count()
            .expect("two search occurrences")
    );

    repository
        .decide_metadata_assertion(
            collection_id,
            MetadataField::Parody,
            assertion_id,
            MetadataAssertionDecision::Select,
        )
        .expect("select suggestion");
    let repeated_after_selection = repository
        .save_external_candidate(candidate())
        .expect("repeat selected suggestion");
    assert!(matches!(
        repeated_after_selection,
        ExternalCandidateOutcome::Suggestion {
            assertion_id: reused_id,
            ..
        } if reused_id == assertion_id
    ));
    assert_eq!(
        assertion_id,
        repository
            .current_selection(collection_id, MetadataField::Parody)
            .expect("parody selection")
            .expect("selected parody")
            .assertion_id
    );

    repository
        .decide_metadata_assertion(
            collection_id,
            MetadataField::Parody,
            assertion_id,
            MetadataAssertionDecision::Reject,
        )
        .expect("reject suggestion");
    assert!(matches!(
        repository
            .save_external_candidate(candidate())
            .expect("repeat rejected result"),
        ExternalCandidateOutcome::SearchOnly { .. }
    ));
    assert_eq!(
        assertions_before + 1,
        repository
            .assertion_count()
            .expect("rejection does not create assertion")
    );
    let history = repository
        .metadata_history(collection_id)
        .expect("metadata history");
    let parody = history
        .fields
        .iter()
        .find(|field| field.field == MetadataField::Parody)
        .expect("parody history");
    assert_eq!(1, parody.assertions.len());
    assert_eq!(4, parody.external_search_results.len());
    assert_eq!(
        MetadataAssertionStatus::Rejected,
        parody.assertions[0].status
    );
    assert_eq!(
        ExternalSearchDisposition::SearchOnly,
        parody.external_search_results[0].disposition
    );
}

#[test]
fn automatic_external_value_never_overwrites_a_manual_decision() {
    let tree = TestTree::new("manual-conflict");
    let pending = tree.pending("(C77) [circle] title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    assert!(matches!(
        repository
            .save_external_candidate(ExternalCandidate {
                collection_id,
                field: MetadataField::Event,
                value: MetadataValue::Text("accepted external event".to_owned()),
                source_reference: "provider:accepted-before-manual".to_owned(),
                confidence: confidence(0.99, true),
            })
            .expect("auto apply initial external event"),
        ExternalCandidateOutcome::AutoApplied { .. }
    ));
    repository
        .set_manual_value(
            collection_id,
            MetadataField::Event,
            MetadataValue::Text("manual event".to_owned()),
        )
        .expect("set manual event");

    let outcome = repository
        .save_external_candidate(ExternalCandidate {
            collection_id,
            field: MetadataField::Event,
            value: MetadataValue::Text("external event".to_owned()),
            source_reference: "provider:conflict".to_owned(),
            confidence: confidence(0.99, true),
        })
        .expect("save conflicting external candidate");

    assert!(matches!(
        outcome,
        ExternalCandidateOutcome::Suggestion { .. }
    ));
    let current = repository
        .current_selection(collection_id, MetadataField::Event)
        .expect("event selection")
        .expect("event");
    assert_eq!(MetadataSource::Manual, current.source);
    assert_eq!("\"manual event\"", current.value_json);

    assert!(
        repository
            .clear_manual_value(collection_id, MetadataField::Event)
            .expect("clear manual event")
    );
    let restored = repository
        .current_selection(collection_id, MetadataField::Event)
        .expect("restored event selection")
        .expect("restored event");
    assert_eq!(MetadataSource::External, restored.source);
    assert_eq!("\"accepted external event\"", restored.value_json);
}

#[test]
fn metadata_history_exposes_selection_sources_confidence_and_search_only_results() {
    let tree = TestTree::new("metadata-history");
    let pending = tree.pending("[circle] filename title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    repository
        .set_inferred_value(
            collection_id,
            MetadataField::Title,
            MetadataValue::Text("inferred title".to_owned()),
            "local inference",
        )
        .expect("set inference");
    let medium = repository
        .save_external_candidate(ExternalCandidate {
            collection_id,
            field: MetadataField::Title,
            value: MetadataValue::Text("external suggestion".to_owned()),
            source_reference: "provider:medium".to_owned(),
            confidence: confidence(0.8, false),
        })
        .expect("save suggestion");
    let ExternalCandidateOutcome::Suggestion {
        assertion_id: external_assertion_id,
        ..
    } = medium
    else {
        panic!("medium candidate must be a suggestion");
    };
    assert!(matches!(
        repository
            .save_external_candidate(ExternalCandidate {
                collection_id,
                field: MetadataField::Title,
                value: MetadataValue::Text("search-only title".to_owned()),
                source_reference: "provider:low".to_owned(),
                confidence: confidence(0.5, false),
            })
            .expect("save search-only result"),
        ExternalCandidateOutcome::SearchOnly { .. }
    ));
    let manual_assertion_id = repository
        .set_manual_value(
            collection_id,
            MetadataField::Title,
            MetadataValue::Text("manual title".to_owned()),
        )
        .expect("set manual title");

    let history = repository
        .metadata_history(collection_id)
        .expect("metadata history");
    assert_eq!(7, history.fields.len());
    let title = history
        .fields
        .iter()
        .find(|history| history.field == MetadataField::Title)
        .expect("title history");
    assert_eq!(4, title.assertions.len());
    assert_eq!(
        manual_assertion_id,
        title
            .selection
            .as_ref()
            .expect("title selection")
            .assertion_id
    );
    assert_eq!(
        HashSet::from([
            MetadataSource::Manual,
            MetadataSource::External,
            MetadataSource::Filename,
            MetadataSource::Inference,
        ]),
        title
            .assertions
            .iter()
            .map(|assertion| assertion.source)
            .collect()
    );
    let external = title
        .assertions
        .iter()
        .find(|assertion| assertion.id == external_assertion_id)
        .expect("external assertion");
    assert_eq!(MetadataAssertionStatus::Candidate, external.status);
    assert_eq!(Some(0.8), external.confidence_total);
    assert_eq!(
        Some("provider:medium"),
        external.source_reference.as_deref()
    );
    let confidence_json: serde_json::Value = serde_json::from_str(
        external
            .confidence_json
            .as_deref()
            .expect("confidence JSON"),
    )
    .expect("decode confidence");
    assert_eq!(0.95, confidence_json["source_reliability"]);
    assert_eq!("測試用外部來源證據", confidence_json["reason"]);
    assert_eq!(2, title.external_search_results.len());
    assert_eq!(
        HashSet::from([
            ExternalSearchDisposition::Suggestion,
            ExternalSearchDisposition::SearchOnly,
        ]),
        title
            .external_search_results
            .iter()
            .map(|result| result.disposition)
            .collect()
    );
    let search_only = title
        .external_search_results
        .iter()
        .find(|result| result.disposition == ExternalSearchDisposition::SearchOnly)
        .expect("search-only result");
    assert_eq!(None, search_only.assertion_id);
    assert_eq!("provider:low", search_only.source_reference);

    repository
        .clear_manual_value(collection_id, MetadataField::Title)
        .expect("clear manual title");
    let restored = repository
        .metadata_history(collection_id)
        .expect("restored history");
    let title = restored
        .fields
        .iter()
        .find(|history| history.field == MetadataField::Title)
        .expect("restored title history");
    let selected_id = title
        .selection
        .as_ref()
        .expect("restored selection")
        .assertion_id;
    assert_eq!(
        MetadataSource::Filename,
        title
            .assertions
            .iter()
            .find(|assertion| assertion.id == selected_id)
            .expect("selected assertion")
            .source
    );
    assert_eq!(
        MetadataAssertionStatus::Obsolete,
        title
            .assertions
            .iter()
            .find(|assertion| assertion.id == manual_assertion_id)
            .expect("manual history retained")
            .status
    );
}

#[test]
fn manual_assertion_decisions_validate_ownership_and_preserve_rejected_history() {
    let tree = TestTree::new("metadata-decisions");
    let first = tree.pending("[circle] filename title.zip");
    let second = tree.pending("[other] second title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&first)
        .expect("ingest first collection");
    repository
        .ingest_collection(&second)
        .expect("ingest second collection");
    let collection_id = repository
        .collection_id_for_current_path(&first.path)
        .expect("first collection lookup")
        .expect("first collection ID");
    let other_collection_id = repository
        .collection_id_for_current_path(&second.path)
        .expect("second collection lookup")
        .expect("second collection ID");
    let outcome = repository
        .save_external_candidate(ExternalCandidate {
            collection_id,
            field: MetadataField::Title,
            value: MetadataValue::Text("external title".to_owned()),
            source_reference: "provider:decision".to_owned(),
            confidence: confidence(0.8, false),
        })
        .expect("save external suggestion");
    let ExternalCandidateOutcome::Suggestion { assertion_id, .. } = outcome else {
        panic!("medium-confidence external result must remain a suggestion");
    };
    let nonselected = repository
        .save_external_candidate(ExternalCandidate {
            collection_id,
            field: MetadataField::Title,
            value: MetadataValue::Text("other suggestion".to_owned()),
            source_reference: "provider:other".to_owned(),
            confidence: confidence(0.82, false),
        })
        .expect("save non-selected suggestion");
    let ExternalCandidateOutcome::Suggestion {
        assertion_id: nonselected_assertion_id,
        ..
    } = nonselected
    else {
        panic!("second medium-confidence result must remain a suggestion");
    };
    repository
        .decide_metadata_assertion(
            collection_id,
            MetadataField::Title,
            nonselected_assertion_id,
            MetadataAssertionDecision::Reject,
        )
        .expect("reject non-selected suggestion");
    assert_eq!(
        MetadataSource::Filename,
        repository
            .current_selection(collection_id, MetadataField::Title)
            .expect("selection after rejecting non-selected suggestion")
            .expect("filename title")
            .source
    );

    for (owner, field) in [
        (other_collection_id, MetadataField::Title),
        (collection_id, MetadataField::Event),
    ] {
        let error = repository
            .decide_metadata_assertion(
                owner,
                field,
                assertion_id,
                MetadataAssertionDecision::Select,
            )
            .expect_err("cross-owner decision must fail");
        assert!(matches!(
            error,
            StorageError::AssertionUnavailable(id) if id == assertion_id
        ));
    }
    assert_eq!(
        MetadataSource::Filename,
        repository
            .current_selection(collection_id, MetadataField::Title)
            .expect("initial selection")
            .expect("filename title")
            .source
    );

    repository
        .decide_metadata_assertion(
            collection_id,
            MetadataField::Title,
            assertion_id,
            MetadataAssertionDecision::Select,
        )
        .expect("select external candidate");
    let selected = repository
        .current_selection(collection_id, MetadataField::Title)
        .expect("selected external")
        .expect("external title");
    assert_eq!(assertion_id, selected.assertion_id);
    assert_eq!(MetadataSource::External, selected.source);
    assert!(selected.selected_manually);

    repository
        .decide_metadata_assertion(
            collection_id,
            MetadataField::Title,
            assertion_id,
            MetadataAssertionDecision::Reject,
        )
        .expect("reject selected external assertion");
    repository
        .decide_metadata_assertion(
            collection_id,
            MetadataField::Title,
            assertion_id,
            MetadataAssertionDecision::Reject,
        )
        .expect("repeated rejection is idempotent");
    assert_eq!(
        MetadataSource::Filename,
        repository
            .current_selection(collection_id, MetadataField::Title)
            .expect("fallback selection")
            .expect("filename fallback")
            .source
    );
    let history = repository
        .metadata_history(collection_id)
        .expect("metadata history");
    let rejected = history
        .fields
        .iter()
        .find(|field| field.field == MetadataField::Title)
        .expect("title history")
        .assertions
        .iter()
        .find(|assertion| assertion.id == assertion_id)
        .expect("rejected assertion history");
    assert_eq!(MetadataAssertionStatus::Rejected, rejected.status);
    assert_eq!(Some("測試用外部來源證據"), rejected.reason.as_deref());
    assert!(rejected.confidence_json.is_some());

    let error = repository
        .decide_metadata_assertion(
            collection_id,
            MetadataField::Title,
            assertion_id,
            MetadataAssertionDecision::Select,
        )
        .expect_err("rejected assertion cannot be selected again");
    assert!(matches!(
        error,
        StorageError::AssertionUnavailable(id) if id == assertion_id
    ));
}

#[test]
fn filename_value_beats_inference_and_projection_can_be_rebuilt() {
    let tree = TestTree::new("rebuild");
    let pending = tree.pending("[circle] filename title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    repository
        .set_inferred_value(
            collection_id,
            MetadataField::Title,
            MetadataValue::Text("inferred title".to_owned()),
            "測試推斷",
        )
        .expect("save inference");

    assert_eq!(
        MetadataSource::Filename,
        repository
            .current_selection(collection_id, MetadataField::Title)
            .expect("selection")
            .expect("title")
            .source
    );
    assert_eq!(
        1,
        repository
            .rebuild_all_projections()
            .expect("rebuild projections")
    );
    assert_eq!(
        vec!["filename title".to_owned()],
        repository.search_titles("filename").expect("rebuilt FTS")
    );
}

#[test]
fn invalid_typed_metadata_is_rejected_without_writes() {
    let tree = TestTree::new("invalid-metadata");
    let pending = tree.pending("[circle] title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    let before = repository.assertion_count().expect("assertion count");

    assert!(matches!(
        repository.set_manual_value(
            collection_id,
            MetadataField::Title,
            MetadataValue::Boolean(true),
        ),
        Err(StorageError::InvalidMetadata(_))
    ));
    assert!(matches!(
        repository.set_manual_value(
            collection_id,
            MetadataField::Title,
            MetadataValue::Text("   ".to_owned()),
        ),
        Err(StorageError::InvalidMetadata(_))
    ));
    assert_eq!(before, repository.assertion_count().expect("no writes"));
}

#[test]
fn canonical_mapping_changes_projection_without_rewriting_raw_assertion() {
    let tree = TestTree::new("canonical-parody");
    let mut pending = tree.pending("[circle] canonical searchable.zip");
    pending.parsed.parody = Some(Parody {
        raw: "PokemonAlias".to_owned(),
        canonical: "PokemonAlias".to_owned(),
        evidence: "filename_dictionary".to_owned(),
    });
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    let assertion = repository
        .current_selection(collection_id, MetadataField::Parody)
        .expect("parody selection")
        .expect("parody");
    let raw_payload = assertion.value_json.clone();
    let entity_id = repository
        .create_canonical_entity(EntityKind::Parody, "PocketMonsters", true)
        .expect("create canonical entity");

    repository
        .map_assertion_to_canonical(
            assertion.assertion_id,
            0,
            "PokemonAlias",
            entity_id,
            &mapping_evidence("官方原作名稱"),
        )
        .expect("map parody");

    assert_eq!(
        vec!["canonical searchable".to_owned()],
        repository
            .search_titles("PocketMonsters")
            .expect("canonical FTS")
    );
    assert_eq!(
        raw_payload,
        repository
            .current_selection(collection_id, MetadataField::Parody)
            .expect("raw selection")
            .expect("parody")
            .value_json
    );
    let entity = repository
        .canonical_entity(entity_id)
        .expect("canonical entity");
    assert!(entity.is_official);
    assert_eq!("PocketMonsters", entity.canonical_name);

    repository
        .update_canonical_entity(entity_id, "PokemonOfficial", true)
        .expect("rename canonical entity");
    assert!(
        repository
            .search_titles("PocketMonsters")
            .expect("old canonical search")
            .is_empty()
    );
    assert_eq!(
        vec!["canonical searchable".to_owned()],
        repository
            .search_titles("PokemonOfficial")
            .expect("updated canonical FTS")
    );
    assert_eq!(
        raw_payload,
        repository
            .current_selection(collection_id, MetadataField::Parody)
            .expect("raw selection after rename")
            .expect("parody")
            .value_json
    );
}

#[test]
fn author_canonical_mapping_preserves_list_order_and_allows_partial_mapping() {
    let tree = TestTree::new("canonical-authors");
    let pending = tree.pending("[circle (RawAuthorA、RawAuthorB)] title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    let assertion_id = repository
        .current_selection(collection_id, MetadataField::Authors)
        .expect("authors selection")
        .expect("authors")
        .assertion_id;
    let first = repository
        .create_canonical_entity(EntityKind::Author, "CanonicalAuthorA", false)
        .expect("create first author");
    let second = repository
        .create_canonical_entity(EntityKind::Author, "CanonicalAuthorB", true)
        .expect("create second author");

    repository
        .map_assertion_to_canonical(
            assertion_id,
            1,
            "RawAuthorB",
            second,
            &mapping_evidence("第二位作者"),
        )
        .expect("map second author");
    assert_eq!(
        vec!["RawAuthorA".to_owned(), "CanonicalAuthorB".to_owned()],
        repository
            .effective_authors(collection_id)
            .expect("partial canonical authors")
    );

    repository
        .map_assertion_to_canonical(
            assertion_id,
            0,
            "RawAuthorA",
            first,
            &mapping_evidence("第一位作者"),
        )
        .expect("map first author");
    assert_eq!(
        vec!["CanonicalAuthorA".to_owned(), "CanonicalAuthorB".to_owned()],
        repository
            .effective_authors(collection_id)
            .expect("canonical authors")
    );
}

#[test]
fn invalid_canonical_mapping_rolls_back_without_changing_projection() {
    let tree = TestTree::new("canonical-invalid");
    let pending = tree.pending("[circle (RawAuthor)] title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    let assertion_id = repository
        .current_selection(collection_id, MetadataField::Authors)
        .expect("authors selection")
        .expect("authors")
        .assertion_id;
    let wrong_kind = repository
        .create_canonical_entity(EntityKind::Circle, "WrongKind", false)
        .expect("create wrong kind");

    assert!(matches!(
        repository.map_assertion_to_canonical(
            assertion_id,
            0,
            "RawAuthor",
            wrong_kind,
            &mapping_evidence("錯誤 kind"),
        ),
        Err(StorageError::InvalidCanonicalMapping(_))
    ));
    assert!(matches!(
        repository.map_assertion_to_canonical(
            assertion_id,
            2,
            "RawAuthor",
            wrong_kind,
            &mapping_evidence("錯誤 index"),
        ),
        Err(StorageError::InvalidCanonicalMapping(_))
    ));
    assert_eq!(
        vec!["RawAuthor".to_owned()],
        repository
            .effective_authors(collection_id)
            .expect("unchanged authors")
    );
}

#[test]
fn rejected_merge_pair_is_symmetric_and_removable() {
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    let first = repository
        .create_canonical_entity(EntityKind::Parody, "First", false)
        .expect("create first entity");
    let second = repository
        .create_canonical_entity(EntityKind::Parody, "Second", false)
        .expect("create second entity");

    repository
        .exclude_entity_merge(second, first, "收藏管理者確認不是同一原作")
        .expect("exclude merge");

    assert!(
        repository
            .is_entity_merge_excluded(first, second)
            .expect("forward exclusion")
    );
    assert!(
        repository
            .is_entity_merge_excluded(second, first)
            .expect("reverse exclusion")
    );
    assert!(matches!(
        repository.delete_canonical_entity(first),
        Err(StorageError::CanonicalEntityInUse(id)) if id == first
    ));
    assert!(
        repository
            .remove_entity_merge_exclusion(second, first)
            .expect("remove exclusion")
    );
    assert!(
        !repository
            .is_entity_merge_excluded(first, second)
            .expect("exclusion removed")
    );
}

#[test]
fn canonical_entity_cannot_be_deleted_until_mappings_and_aliases_are_removed() {
    let tree = TestTree::new("canonical-delete");
    let pending = tree.pending("[RawCircle] title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .first_collection_id()
        .expect("collection query")
        .expect("collection");
    let assertion_id = repository
        .current_selection(collection_id, MetadataField::Circle)
        .expect("circle selection")
        .expect("circle")
        .assertion_id;
    let entity_id = repository
        .create_canonical_entity(EntityKind::Circle, "CanonicalCircle", true)
        .expect("create circle entity");
    repository
        .map_assertion_to_canonical(
            assertion_id,
            0,
            "RawCircle",
            entity_id,
            &mapping_evidence("官方社團名稱"),
        )
        .expect("map circle");

    assert!(matches!(
        repository.delete_canonical_entity(entity_id),
        Err(StorageError::CanonicalEntityInUse(id)) if id == entity_id
    ));
    assert!(
        repository
            .remove_assertion_canonical_mapping(assertion_id, 0)
            .expect("remove mapping")
    );
    assert!(matches!(
        repository.delete_canonical_entity(entity_id),
        Err(StorageError::CanonicalEntityInUse(id)) if id == entity_id
    ));
    assert!(
        repository
            .remove_name_variant(entity_id, "RawCircle")
            .expect("remove alias")
    );
    repository
        .delete_canonical_entity(entity_id)
        .expect("delete unreferenced entity");
    assert!(matches!(
        repository.canonical_entity(entity_id),
        Err(StorageError::CanonicalEntityNotFound(id)) if id == entity_id
    ));
    assert_eq!(
        vec!["title".to_owned()],
        repository
            .search_titles("RawCircle")
            .expect("raw circle restored in FTS")
    );
}

#[test]
fn vocabulary_candidates_cover_four_fields_with_safe_normalization_and_counts() {
    let tree = TestTree::new("vocabulary-four-fields");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");

    for (field, values) in [
        (VocabularyField::Event, ["ＡＬＩＣＥ", "alice"]),
        (VocabularyField::Circle, ["Circle・Works", "circle works"]),
        (VocabularyField::Author, ["トウホウ", "とうほう"]),
        (VocabularyField::Parody, ["Project‐Moon", "project moon"]),
    ] {
        for (index, value) in values.into_iter().enumerate() {
            let pending = tree.pending(&format!(
                "[seed-{field:?}-{index}] title-{field:?}-{index}.zip"
            ));
            let path = pending.path.clone();
            repository
                .ingest_collection(&pending)
                .expect("ingest collection");
            let collection_id = repository
                .collection_id_for_current_path(&path)
                .expect("collection lookup")
                .expect("collection id");
            let (metadata_field, metadata_value) = match field {
                VocabularyField::Event => {
                    (MetadataField::Event, MetadataValue::Text(value.to_owned()))
                }
                VocabularyField::Circle => {
                    (MetadataField::Circle, MetadataValue::Text(value.to_owned()))
                }
                VocabularyField::Author => (
                    MetadataField::Authors,
                    MetadataValue::Authors(Authors {
                        raw: Some(value.to_owned()),
                        values: vec![value.to_owned()],
                    }),
                ),
                VocabularyField::Parody => (
                    MetadataField::Parody,
                    MetadataValue::Parody(Parody {
                        raw: value.to_owned(),
                        canonical: value.to_owned(),
                        evidence: "manual_test".to_owned(),
                    }),
                ),
            };
            repository
                .set_manual_value(collection_id, metadata_field, metadata_value)
                .expect("set vocabulary value");
        }

        let groups = repository
            .vocabulary_candidates(Some(field))
            .expect("vocabulary candidates");
        assert_eq!(1, groups.len(), "{field:?}");
        assert_eq!(2, groups[0].variants.len(), "{field:?}");
        assert!(
            groups[0]
                .variants
                .iter()
                .all(|variant| variant.active_count == 1 && !variant.representatives.is_empty()),
            "{field:?}"
        );
    }
}

#[test]
fn vocabulary_suggestions_cover_fields_aliases_counts_search_limit_and_active_current_scope() {
    let tree = TestTree::new("vocabulary-suggestions");
    let database = tree.database();
    let mut repository = CatalogRepository::open(&database).expect("open catalog");
    let rows = [
        ("Top", "Top", "Top", "Top"),
        ("top", "top", "top", "top"),
        ("Rare", "Rare", "Rare", "Rare"),
        ("Inactive", "Inactive", "Inactive", "Inactive"),
        ("No Current", "No Current", "No Current", "No Current"),
    ];
    let mut collection_ids = Vec::new();
    for (index, (event, circle, author, parody)) in rows.into_iter().enumerate() {
        let pending = tree.pending(&format!("[suggest-{index}] suggestion-{index}.zip"));
        let path = pending.path.clone();
        repository
            .ingest_collection(&pending)
            .expect("ingest suggestion collection");
        let collection_id = repository
            .collection_id_for_current_path(&path)
            .expect("suggestion lookup")
            .expect("suggestion collection");
        for (field, value) in [
            (
                MetadataField::Event,
                MetadataValue::Text(format!("Event {event}")),
            ),
            (
                MetadataField::Circle,
                MetadataValue::Text(format!("Circle {circle}")),
            ),
            (
                MetadataField::Authors,
                MetadataValue::Authors(Authors {
                    raw: Some(format!("Author {author}")),
                    values: vec![format!("Author {author}")],
                }),
            ),
            (
                MetadataField::Parody,
                MetadataValue::Parody(Parody {
                    raw: format!("Parody {parody}"),
                    canonical: format!("Parody {parody}"),
                    evidence: "suggestion test".to_owned(),
                }),
            ),
        ] {
            repository
                .set_manual_value(collection_id, field, value)
                .expect("set suggestion value");
        }
        collection_ids.push(collection_id);
    }
    drop(repository);

    let connection = Connection::open(&database).expect("open suggestion fixture database");
    connection
        .execute(
            "UPDATE collections SET status = 'tombstone' WHERE id = ?1",
            [collection_ids[3]],
        )
        .expect("make suggestion collection inactive");
    connection
        .execute(
            "UPDATE collection_locations SET location_status = 'missing', ended_at = CURRENT_TIMESTAMP
             WHERE collection_id = ?1 AND location_status = 'current'",
            [collection_ids[4]],
        )
        .expect("remove current suggestion location");
    drop(connection);

    let mut repository = CatalogRepository::open(&database).expect("reopen suggestion catalog");
    for (field, prefix) in [
        (VocabularyField::Event, "Event"),
        (VocabularyField::Circle, "Circle"),
        (VocabularyField::Author, "Author"),
        (VocabularyField::Parody, "Parody"),
    ] {
        let items = repository
            .vocabulary_suggestions(field, "", 50)
            .expect("top vocabulary suggestions");
        assert_eq!(2, items.len(), "{field:?}");
        assert_eq!(
            (format!("{prefix} Top"), 2),
            (items[0].name.clone(), items[0].count)
        );
        assert_eq!(
            (format!("{prefix} Rare"), 1),
            (items[1].name.clone(), items[1].count)
        );
        assert_eq!(
            vec![format!("{prefix} Rare")],
            repository
                .vocabulary_suggestions(field, "rare", 50)
                .expect("search vocabulary suggestions")
                .into_iter()
                .map(|item| item.name)
                .collect::<Vec<_>>(),
            "{field:?}"
        );
        let case_search = repository
            .vocabulary_suggestions(field, "TOP", 50)
            .expect("search case-only vocabulary suggestions");
        assert_eq!(1, case_search.len(), "{field:?}");
        assert_eq!(format!("{prefix} Top"), case_search[0].name, "{field:?}");
        assert_eq!(2, case_search[0].count, "{field:?}");
        assert_eq!(
            1,
            repository
                .vocabulary_suggestions(field, "", 1)
                .expect("limit vocabulary suggestions")
                .len(),
            "{field:?}"
        );
    }

    repository
        .merge_vocabulary(
            VocabularyField::Event,
            "Event Canonical",
            &[
                "Event Canonical".to_owned(),
                "Event Top".to_owned(),
                "Event top".to_owned(),
                "Blue Event Alias".to_owned(),
            ],
        )
        .expect("create suggestion aliases");
    let alias_items = repository
        .vocabulary_suggestions(VocabularyField::Event, "blue", 20)
        .expect("search suggestion alias");
    assert_eq!(1, alias_items.len());
    assert_eq!("Event Canonical", alias_items[0].name);
    assert_eq!(2, alias_items[0].count);
    assert_eq!(
        vec![
            "Blue Event Alias".to_owned(),
            "Event Top".to_owned(),
            "Event top".to_owned(),
        ],
        alias_items[0].aliases
    );
    assert_eq!(
        "Event Canonical",
        repository
            .vocabulary_suggestions(VocabularyField::Event, "canonical", 20)
            .expect("search canonical suggestion")[0]
            .name
    );

    repository
        .set_manual_value(
            collection_ids[2],
            MetadataField::Event,
            MetadataValue::Text("Event Canonical".to_owned()),
        )
        .expect("write selected canonical manually");
    assert_eq!(
        Some("Event Canonical".to_owned()),
        repository
            .collection(collection_ids[2])
            .expect("canonical manual collection")
            .event
    );
}

#[test]
fn vocabulary_suggestions_merge_mapped_and_unmapped_nocase_names_with_mapped_priority() {
    let tree = TestTree::new("vocabulary-suggestion-mapped-nocase");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    for (index, case) in ["Top", "top"].into_iter().enumerate() {
        let pending = tree.pending(&format!("[mapped-case-{index}] mapped-case-{index}.zip"));
        let path = pending.path.clone();
        repository
            .ingest_collection(&pending)
            .expect("ingest mapped case collection");
        let collection_id = repository
            .collection_id_for_current_path(&path)
            .expect("mapped case lookup")
            .expect("mapped case collection");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Event,
                MetadataValue::Text(format!("Mapped {case}")),
            )
            .expect("set mapped case event");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Circle,
                MetadataValue::Text(format!("Entity {case}")),
            )
            .expect("set entity case circle");
    }

    repository
        .merge_vocabulary(
            VocabularyField::Event,
            "Mapped Top",
            &["Mapped Top".to_owned(), "Mapped Alias".to_owned()],
        )
        .expect("map only uppercase event");
    let mapped_and_unmapped = repository
        .vocabulary_suggestions(VocabularyField::Event, "alias", 20)
        .expect("search mapped and unmapped event");
    assert_eq!(1, mapped_and_unmapped.len());
    assert_eq!("Mapped Top", mapped_and_unmapped[0].name);
    assert_eq!(2, mapped_and_unmapped[0].count);
    assert_eq!(vec!["Mapped Alias"], mapped_and_unmapped[0].aliases);

    repository
        .create_canonical_entity(EntityKind::Circle, "Entity Top", false)
        .expect("create uppercase circle entity");
    repository
        .create_canonical_entity(EntityKind::Circle, "Entity top", false)
        .expect("create lowercase circle entity");
    let separate_entities = repository
        .vocabulary_suggestions(VocabularyField::Circle, "ENTITY TOP", 20)
        .expect("search separate nocase entities");
    assert_eq!(1, separate_entities.len());
    assert_eq!("Entity Top", separate_entities[0].name);
    assert_eq!(2, separate_entities[0].count);
    assert!(separate_entities[0].aliases.is_empty());
}

#[test]
fn vocabulary_merge_preserves_manual_priority_and_updates_library_and_saved_views() {
    let tree = TestTree::new("vocabulary-merge");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut collection_ids = Vec::new();
    for (index, value) in ["Ｃ１００", "C100"].into_iter().enumerate() {
        let pending = tree.pending(&format!("[circle-{index}] event-{index}.zip"));
        let path = pending.path.clone();
        repository
            .ingest_collection(&pending)
            .expect("ingest collection");
        let collection_id = repository
            .collection_id_for_current_path(&path)
            .expect("collection lookup")
            .expect("collection id");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Event,
                MetadataValue::Text(value.to_owned()),
            )
            .expect("set event");
        collection_ids.push(collection_id);
    }
    let saved_query = SavedViewQuery::from_collection_query(
        &CollectionQuery {
            filters: CollectionFilters {
                event: Some("Ｃ１００".to_owned()),
                ..CollectionFilters::default()
            },
            ..CollectionQuery::default()
        },
        SavedViewLayout::Grid,
    );
    let saved = repository
        .create_saved_view("舊活動名稱", &saved_query, true)
        .expect("save old event view");
    let variants = vec!["Ｃ１００".to_owned(), "C100".to_owned()];

    let preflight = repository
        .vocabulary_merge_preflight(VocabularyField::Event, "C100", &variants)
        .expect("merge preflight");
    assert_eq!(2, preflight.affected_collections);
    assert_eq!(2, preflight.manual_assertions);
    assert_eq!(1, preflight.manual_selected_conflicts);
    assert_eq!(
        vec![saved.id],
        preflight
            .saved_views
            .iter()
            .map(|view| view.id)
            .collect::<Vec<_>>()
    );

    let result = repository
        .merge_vocabulary(VocabularyField::Event, "C100", &variants)
        .expect("merge vocabulary");
    assert_eq!(2, result.affected_collections);
    assert_eq!(1, result.saved_views_updated);
    let original_selection = repository
        .current_selection(collection_ids[0], MetadataField::Event)
        .expect("selection")
        .expect("selected event");
    assert!(original_selection.selected_manually);
    assert_eq!(
        serde_json::json!("Ｃ１００").to_string(),
        original_selection.value_json
    );
    assert_eq!(
        Some("C100".to_owned()),
        repository
            .collection(collection_ids[0])
            .expect("collection")
            .event
    );
    assert_eq!(
        Some("C100".to_owned()),
        repository
            .saved_view(saved.id)
            .expect("saved view")
            .query
            .filters
            .event
    );

    let filtered = repository
        .collections(&CollectionQuery {
            filters: CollectionFilters {
                event: Some("C100".to_owned()),
                ..CollectionFilters::default()
            },
            ..CollectionQuery::default()
        })
        .expect("canonical filter");
    assert_eq!(2, filtered.total);
    let facets = repository
        .collection_facets(CollectionFacet::Event, "C100", 20)
        .expect("canonical facets");
    assert_eq!(("C100", 2), (facets[0].name.as_str(), facets[0].count));
    assert!(
        repository
            .collection_statistics()
            .expect("statistics")
            .top_events
            .iter()
            .any(|entry| entry.name == "C100" && entry.count == 2)
    );

    let pending = tree.pending("[future] future-alias.zip");
    let path = pending.path.clone();
    repository
        .ingest_collection(&pending)
        .expect("ingest future collection");
    let future_id = repository
        .collection_id_for_current_path(&path)
        .expect("future lookup")
        .expect("future id");
    repository
        .set_inferred_value(
            future_id,
            MetadataField::Event,
            MetadataValue::Text("Ｃ１００".to_owned()),
            "test alias reuse",
        )
        .expect("save inferred alias");
    assert_eq!(
        Some("C100".to_owned()),
        repository
            .collection(future_id)
            .expect("future collection")
            .event
    );
    repository
        .set_manual_value(
            future_id,
            MetadataField::Event,
            MetadataValue::Text("Ｃ１００".to_owned()),
        )
        .expect("manual alias remains protected");
    assert_eq!(
        Some("Ｃ１００".to_owned()),
        repository
            .collection(future_id)
            .expect("manual collection")
            .event
    );
}

#[test]
fn rejected_vocabulary_pair_is_persistent_and_not_suggested_again() {
    let tree = TestTree::new("vocabulary-reject");
    let database = tree.database();
    let mut repository = CatalogRepository::open(&database).expect("open catalog");
    for (index, value) in ["Circle・Name", "circle name"].into_iter().enumerate() {
        let pending = tree.pending(&format!("[reject-{index}] reject-{index}.zip"));
        let path = pending.path.clone();
        repository
            .ingest_collection(&pending)
            .expect("ingest collection");
        let collection_id = repository
            .collection_id_for_current_path(&path)
            .expect("collection lookup")
            .expect("collection id");
        repository
            .set_manual_value(
                collection_id,
                MetadataField::Circle,
                MetadataValue::Text(value.to_owned()),
            )
            .expect("set circle");
    }
    assert_eq!(
        1,
        repository
            .vocabulary_candidates(Some(VocabularyField::Circle))
            .expect("candidate")
            .len()
    );
    repository
        .reject_vocabulary_group(
            VocabularyField::Circle,
            &["Circle・Name".to_owned(), "circle name".to_owned()],
            "管理者確認為不同社團",
            false,
        )
        .expect("reject candidate");
    drop(repository);

    let repository = CatalogRepository::open(&database).expect("reopen catalog");
    assert!(
        repository
            .vocabulary_candidates(Some(VocabularyField::Circle))
            .expect("persistent candidates")
            .is_empty()
    );
}

#[test]
fn completed_system_move_keeps_collection_identity_and_location_history() {
    let tree = TestTree::new("system-move");
    let pending = tree.pending_under(
        "downloads",
        SourceKind::Downloads,
        "[circle] movable title.zip",
    );
    let source_path = pending.path.clone();
    let archive_root = tree.path.join("archive");
    fs::create_dir(&archive_root).expect("create archive root");
    let destination = archive_root.join("C106/[circle] movable title.zip");
    fs::create_dir_all(destination.parent().expect("destination parent"))
        .expect("create destination folder");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest downloads collection");
    let collection_id = repository
        .collection_id_for_current_path(&source_path)
        .expect("source lookup")
        .expect("source collection");
    let title_before = repository
        .current_selection(collection_id, MetadataField::Title)
        .expect("title selection")
        .expect("title");
    let assertions_before = repository.assertion_count().expect("assertions");
    let archive_root_id = repository
        .register_library_root(&archive_root, SourceKind::Archive, "歸檔區")
        .expect("register archive root");

    fs::rename(&source_path, &destination).expect("simulate completed file move");
    let operation_id = repository
        .record_completed_system_move(collection_id, archive_root_id, &destination)
        .expect("record completed move");

    assert!(operation_id > 0);
    assert_eq!(
        Some(collection_id),
        repository
            .collection_id_for_current_path(&destination)
            .expect("destination lookup")
    );
    assert_eq!(
        CollectionStatus::Active,
        repository
            .collection_status(collection_id)
            .expect("collection status")
    );
    let history = repository
        .location_history(collection_id)
        .expect("location history");
    assert_eq!(2, history.len());
    assert_eq!(source_path, history[0].path);
    assert_eq!(LocationStatus::Moved, history[0].status);
    assert_eq!(destination, history[1].path);
    assert_eq!(LocationStatus::Current, history[1].status);
    assert_eq!(1, repository.file_operation_count().expect("operations"));
    assert_eq!(
        assertions_before,
        repository.assertion_count().expect("assertions preserved")
    );
    assert_eq!(
        title_before,
        repository
            .current_selection(collection_id, MetadataField::Title)
            .expect("title after move")
            .expect("title")
    );
}

#[test]
fn completed_move_rejects_non_archive_destination_root() {
    let tree = TestTree::new("invalid-move-root");
    let pending = tree.pending_under("downloads", SourceKind::Downloads, "[circle] title.zip");
    let source_path = pending.path.clone();
    let other_downloads = tree.path.join("other-downloads");
    fs::create_dir(&other_downloads).expect("create other downloads root");
    let destination = other_downloads.join("[circle] title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .collection_id_for_current_path(&source_path)
        .expect("source lookup")
        .expect("collection");
    let invalid_root_id = repository
        .register_library_root(&other_downloads, SourceKind::Downloads, "另一下載區")
        .expect("register downloads root");
    fs::rename(&source_path, &destination).expect("simulate file move");

    assert!(matches!(
        repository.record_completed_system_move(collection_id, invalid_root_id, &destination),
        Err(StorageError::InvalidLifecycle(_))
    ));
    assert_eq!(0, repository.file_operation_count().expect("no operation"));
    assert_eq!(
        Some(collection_id),
        repository
            .collection_id_for_current_path(&source_path)
            .expect("database location unchanged")
    );
}

#[test]
fn destructive_operation_rejects_similar_prefix_path_outside_registered_root() {
    let tree = TestTree::new("operation-outside-root");
    let pending = tree.pending_under("library", SourceKind::Downloads, "[circle] inside.zip");
    let database = tree.database();
    let mut repository = CatalogRepository::open(&database).expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .collection_id_for_current_path(&pending.path)
        .expect("collection lookup")
        .expect("collection");
    let outside_directory = tree.path.join("library-other");
    fs::create_dir(&outside_directory).expect("create similar-prefix directory");
    let outside_path = outside_directory.join("[circle] outside.zip");
    fs::write(&outside_path, b"must remain").expect("create outside file");
    Connection::open(&database)
        .expect("open raw connection")
        .execute(
            "UPDATE collection_locations
             SET full_path = ?1, path_key = ?2, relative_path = ?3, filename = ?4
             WHERE collection_id = ?5 AND location_status = 'current'",
            rusqlite::params![
                outside_path.to_string_lossy(),
                path_key(&outside_path),
                "../library-other/[circle] outside.zip",
                "[circle] outside.zip",
                collection_id
            ],
        )
        .expect("corrupt path for boundary test");

    assert!(matches!(
        repository.begin_delete(collection_id, DeleteMode::Permanent),
        Err(StorageError::InvalidLifecycle(message))
            if message.contains("不在目前設定的來源內")
    ));
    assert_eq!(0, repository.file_operation_count().expect("no operation"));
    assert_eq!(
        b"must remain",
        fs::read(&outside_path)
            .expect("outside file remains")
            .as_slice()
    );
}

#[test]
fn destructive_operation_rejects_collection_from_deactivated_root() {
    let tree = TestTree::new("operation-inactive-root");
    let pending = tree.pending_under("downloads", SourceKind::Downloads, "[circle] item.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .collection_id_for_current_path(&pending.path)
        .expect("collection lookup")
        .expect("collection");
    let root_id = repository
        .library_roots()
        .expect("library roots")
        .into_iter()
        .next()
        .expect("downloads root")
        .id;
    repository
        .deactivate_library_root(root_id)
        .expect("deactivate root");

    assert!(matches!(
        repository.begin_delete(collection_id, DeleteMode::Soft),
        Err(StorageError::InvalidLifecycle(message)) if message.contains("來源已停用")
    ));
    assert_eq!(0, repository.file_operation_count().expect("no operation"));
    assert!(pending.path.is_file());
}

#[test]
fn missing_collection_becomes_a_search_hidden_tombstone_with_metadata() {
    let tree = TestTree::new("missing-tombstone");
    let pending = tree.pending_under(
        "downloads",
        SourceKind::Downloads,
        "[circle] missing searchable.zip",
    );
    let path = pending.path.clone();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .collection_id_for_current_path(&path)
        .expect("path lookup")
        .expect("collection");
    let title_before = repository
        .current_selection(collection_id, MetadataField::Title)
        .expect("title selection")
        .expect("title");

    assert!(matches!(
        repository.mark_collection_missing(collection_id),
        Err(StorageError::InvalidLifecycle(_))
    ));
    fs::remove_file(&path).expect("simulate missing file");
    repository
        .mark_collection_missing(collection_id)
        .expect("mark missing");

    assert_eq!(
        CollectionStatus::Tombstone,
        repository
            .collection_status(collection_id)
            .expect("tombstone status")
    );
    assert!(
        repository
            .search_titles("missing")
            .expect("active Library search")
            .is_empty()
    );
    assert_eq!(
        title_before,
        repository
            .current_selection(collection_id, MetadataField::Title)
            .expect("tombstone metadata")
            .expect("title")
    );
    let history = repository
        .location_history(collection_id)
        .expect("location history");
    assert_eq!(LocationStatus::Missing, history[0].status);
    assert!(
        repository
            .collection_id_for_current_path(&path)
            .expect("no current path")
            .is_none()
    );
}

#[test]
fn same_filename_candidates_keep_independent_ids_until_manual_decisions() {
    let tree = TestTree::new("tombstone-candidates");
    let old = tree.pending_under("downloads", SourceKind::Downloads, "[circle] Duplicate.zip");
    let old_path = old.path.clone();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&old)
        .expect("ingest old collection");
    let old_id = repository
        .collection_id_for_current_path(&old_path)
        .expect("old lookup")
        .expect("old collection");
    let old_title = repository
        .current_selection(old_id, MetadataField::Title)
        .expect("old title")
        .expect("old title selection");
    fs::remove_file(&old_path).expect("remove old file");
    repository
        .mark_collection_missing(old_id)
        .expect("mark old missing");

    let first = tree.pending_under("archive-a", SourceKind::Archive, "[circle] Duplicate.zip");
    let second = tree.pending_under("archive-b", SourceKind::Archive, "[circle] Duplicate.zip");
    repository
        .ingest_collection(&first)
        .expect("ingest first candidate");
    repository
        .ingest_collection(&second)
        .expect("ingest second candidate");
    let first_id = repository
        .collection_id_for_current_path(&first.path)
        .expect("first lookup")
        .expect("first candidate");
    let second_id = repository
        .collection_id_for_current_path(&second.path)
        .expect("second lookup")
        .expect("second candidate");

    assert_ne!(old_id, first_id);
    assert_ne!(old_id, second_id);
    assert_ne!(first_id, second_id);
    assert_eq!(
        old_title,
        repository
            .current_selection(old_id, MetadataField::Title)
            .expect("old metadata remains")
            .expect("old title remains")
    );
    let links = repository
        .tombstone_candidates(old_id)
        .expect("candidate links");
    assert_eq!(2, links.len());
    assert!(
        links
            .iter()
            .all(|link| link.decision == CandidateDecision::Pending)
    );
    assert!(links.iter().all(|link| link.reason == "same_filename"));

    repository
        .decide_tombstone_candidate(old_id, first_id, CandidateDecision::Rejected)
        .expect("reject first candidate");
    repository
        .decide_tombstone_candidate(old_id, second_id, CandidateDecision::Confirmed)
        .expect("confirm second candidate");
    let decided = repository
        .tombstone_candidates(old_id)
        .expect("decided links");
    assert_eq!(CandidateDecision::Rejected, decided[0].decision);
    assert_eq!(CandidateDecision::Confirmed, decided[1].decision);
    assert_eq!(
        CollectionStatus::Tombstone,
        repository
            .collection_status(old_id)
            .expect("old stays tombstone until explicit consolidation")
    );
}

#[test]
fn consolidation_requires_manual_conflict_resolution_and_is_idempotent() {
    let tree = TestTree::new("consolidation");
    let filename = "[circle] Consolidate.zip";
    let old = tree.pending_under("old", SourceKind::Downloads, filename);
    let old_path = old.path.clone();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.ingest_collection(&old).expect("ingest old");
    let old_id = repository
        .collection_id_for_current_path(&old_path)
        .expect("old lookup")
        .expect("old ID");
    repository
        .set_manual_value(
            old_id,
            MetadataField::Title,
            MetadataValue::Text("舊手動標題".to_owned()),
        )
        .expect("old manual title");
    repository
        .add_collection_tag(old_id, "old-tag")
        .expect("old tag");
    repository
        .add_to_work_basket(1, &[old_id])
        .expect("basket old identity");
    fs::remove_file(&old_path).expect("remove old file");
    repository
        .mark_collection_missing(old_id)
        .expect("mark old missing");

    let candidate = tree.pending_under("new", SourceKind::Archive, filename);
    let candidate_path = candidate.path.clone();
    repository
        .ingest_collection(&candidate)
        .expect("ingest candidate");
    let candidate_id = repository
        .collection_id_for_current_path(&candidate_path)
        .expect("candidate lookup")
        .expect("candidate ID");
    repository
        .set_manual_value(
            candidate_id,
            MetadataField::Title,
            MetadataValue::Text("新手動標題".to_owned()),
        )
        .expect("candidate manual title");
    repository
        .add_collection_tag(candidate_id, "new-tag")
        .expect("candidate tag");
    repository
        .add_to_work_basket(1, &[candidate_id])
        .expect("basket candidate identity");
    assert_eq!(
        vec![candidate_id],
        repository
            .work_basket(1)
            .expect("active-only basket before consolidation")
            .items
            .iter()
            .map(|item| item.collection.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        1,
        repository.work_baskets().expect("active-only count")[0].count
    );
    repository
        .decide_tombstone_candidate(old_id, candidate_id, CandidateDecision::Confirmed)
        .expect("confirm identity");

    let preflight = repository
        .consolidation_preflight(old_id, candidate_id)
        .expect("preflight");
    assert!(!preflight.ready);
    assert!(preflight.blockers.is_empty());
    assert_eq!(1, preflight.conflicts.len());
    assert_eq!(MetadataField::Title, preflight.conflicts[0].field);
    assert!(matches!(
        repository.consolidate_tombstone_candidate(old_id, candidate_id, &[]),
        Err(StorageError::InvalidLifecycle(_))
    ));
    assert_eq!(
        CollectionStatus::Tombstone,
        repository.collection_status(old_id).expect("old unchanged")
    );
    assert_eq!(
        Some(candidate_id),
        repository
            .collection_id_for_current_path(&candidate_path)
            .expect("candidate location unchanged")
    );

    let completed = repository
        .consolidate_tombstone_candidate(
            old_id,
            candidate_id,
            &[ConsolidationResolution {
                field: MetadataField::Title,
                choice: ConsolidationChoice::Candidate,
            }],
        )
        .expect("consolidate");
    assert!(!completed.already_completed);
    assert_eq!(old_id, completed.survivor_collection_id);
    assert_eq!(candidate_id, completed.merged_collection_id);
    assert_eq!(
        Some(old_id),
        repository
            .collection_id_for_current_path(&candidate_path)
            .expect("survivor owns current path")
    );
    assert_eq!(
        CollectionStatus::Active,
        repository
            .collection_status(old_id)
            .expect("survivor active")
    );
    assert_eq!(
        CollectionStatus::Tombstone,
        repository
            .collection_status(candidate_id)
            .expect("merged candidate hidden")
    );
    assert_eq!(
        Some(old_id),
        repository
            .merged_into_collection(candidate_id)
            .expect("merged redirect")
    );
    let survivor = repository.collection(old_id).expect("survivor detail");
    assert_eq!(Some("新手動標題".to_owned()), survivor.title);
    assert_eq!(vec!["new-tag", "old-tag"], survivor.tags);
    let basket = repository.work_basket(1).expect("consolidated basket");
    assert_eq!(1, basket.items.len());
    assert_eq!(old_id, basket.items[0].collection.id);
    assert_eq!(2, repository.parser_run_count().expect("both parser runs"));
    assert!(
        repository
            .consolidation_transfer_count(completed.consolidation_id)
            .expect("transfer audit")
            >= 5
    );

    let repeated = repository
        .consolidate_tombstone_candidate(old_id, candidate_id, &[])
        .expect("idempotent repeat");
    assert!(repeated.already_completed);
    assert_eq!(completed.consolidation_id, repeated.consolidation_id);
    assert_eq!(2, repository.parser_run_count().expect("no duplicate runs"));
    assert_eq!(
        vec!["new-tag", "old-tag"],
        repository.collection(old_id).expect("survivor").tags
    );
}

#[test]
fn consolidation_waits_until_every_other_candidate_is_rejected() {
    let tree = TestTree::new("consolidation-candidates");
    let filename = "[circle] Multiple.zip";
    let old = tree.pending_under("old", SourceKind::Downloads, filename);
    let old_path = old.path.clone();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.ingest_collection(&old).expect("ingest old");
    let old_id = repository
        .collection_id_for_current_path(&old_path)
        .expect("old lookup")
        .expect("old ID");
    fs::remove_file(&old_path).expect("remove old");
    repository.mark_collection_missing(old_id).expect("missing");
    let first = tree.pending_under("first", SourceKind::Archive, filename);
    let second = tree.pending_under("second", SourceKind::Archive, filename);
    repository.ingest_collection(&first).expect("first");
    repository.ingest_collection(&second).expect("second");
    let first_id = repository
        .collection_id_for_current_path(&first.path)
        .expect("first lookup")
        .expect("first ID");
    let second_id = repository
        .collection_id_for_current_path(&second.path)
        .expect("second lookup")
        .expect("second ID");
    repository
        .decide_tombstone_candidate(old_id, first_id, CandidateDecision::Confirmed)
        .expect("confirm first");

    let blocked = repository
        .consolidation_preflight(old_id, first_id)
        .expect("blocked preflight");
    assert!(!blocked.ready);
    assert!(
        blocked
            .blockers
            .iter()
            .any(|blocker| blocker.kind == "pending_candidates")
    );
    repository
        .decide_tombstone_candidate(old_id, second_id, CandidateDecision::Rejected)
        .expect("reject second");
    let ready = repository
        .consolidation_preflight(old_id, first_id)
        .expect("ready preflight");
    assert!(ready.ready);
    assert!(ready.blockers.is_empty());
    assert!(ready.conflicts.is_empty());
}

#[test]
fn consolidation_failure_rolls_back_identity_location_and_audit() {
    let tree = TestTree::new("consolidation-rollback");
    let filename = "[circle] Rollback.zip";
    let old = tree.pending_under("old", SourceKind::Downloads, filename);
    let old_path = old.path.clone();
    let candidate = tree.pending_under("candidate", SourceKind::Archive, filename);
    let candidate_path = candidate.path.clone();
    let database = tree.database();
    let mut repository = CatalogRepository::open(&database).expect("open catalog");
    repository.ingest_collection(&old).expect("ingest old");
    let old_id = repository
        .collection_id_for_current_path(&old_path)
        .expect("old lookup")
        .expect("old ID");
    fs::remove_file(&old_path).expect("remove old");
    repository.mark_collection_missing(old_id).expect("missing");
    repository
        .ingest_collection(&candidate)
        .expect("ingest candidate");
    let candidate_id = repository
        .collection_id_for_current_path(&candidate_path)
        .expect("candidate lookup")
        .expect("candidate ID");
    repository
        .decide_tombstone_candidate(old_id, candidate_id, CandidateDecision::Confirmed)
        .expect("confirm candidate");
    drop(repository);

    let connection = Connection::open(&database).expect("open trigger connection");
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER reject_consolidation
             BEFORE UPDATE OF status ON collections
             WHEN old.id = {candidate_id} AND new.status = 'tombstone'
             BEGIN
                 SELECT raise(ABORT, 'fixture rejects consolidation');
             END;"
        ))
        .expect("install failure trigger");
    drop(connection);
    let mut repository = CatalogRepository::open(&database).expect("reopen catalog");

    assert!(matches!(
        repository.consolidate_tombstone_candidate(old_id, candidate_id, &[]),
        Err(StorageError::Sqlite(_))
    ));
    assert_eq!(
        CollectionStatus::Tombstone,
        repository.collection_status(old_id).expect("old rollback")
    );
    assert_eq!(
        CollectionStatus::Active,
        repository
            .collection_status(candidate_id)
            .expect("candidate rollback")
    );
    assert_eq!(
        Some(candidate_id),
        repository
            .collection_id_for_current_path(&candidate_path)
            .expect("candidate keeps path")
    );
    assert_eq!(
        None,
        repository
            .merged_into_collection(candidate_id)
            .expect("audit rolled back")
    );
    assert_eq!(2, repository.parser_run_count().expect("runs rolled back"));
}

#[test]
fn consolidation_preserves_tombstone_selection_over_candidate_nonmanual_value() {
    let tree = TestTree::new("consolidation-selection");
    let filename = "[circle] Preserve.zip";
    let old = tree.pending_under("old", SourceKind::Downloads, filename);
    let old_path = old.path.clone();
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.ingest_collection(&old).expect("ingest old");
    let old_id = repository
        .collection_id_for_current_path(&old_path)
        .expect("old lookup")
        .expect("old ID");
    fs::remove_file(&old_path).expect("remove old");
    repository.mark_collection_missing(old_id).expect("missing");
    let candidate = tree.pending_under("candidate", SourceKind::Archive, filename);
    repository
        .ingest_collection(&candidate)
        .expect("ingest candidate");
    let candidate_id = repository
        .collection_id_for_current_path(&candidate.path)
        .expect("candidate lookup")
        .expect("candidate ID");
    repository
        .save_external_candidate(ExternalCandidate {
            collection_id: candidate_id,
            field: MetadataField::Title,
            value: MetadataValue::Text("candidate external title".to_owned()),
            source_reference: "https://example.test/RJ000001".to_owned(),
            confidence: confidence(0.98, true),
        })
        .expect("candidate external title");
    repository
        .decide_tombstone_candidate(old_id, candidate_id, CandidateDecision::Confirmed)
        .expect("confirm");

    repository
        .consolidate_tombstone_candidate(old_id, candidate_id, &[])
        .expect("consolidate");

    assert_eq!(
        Some("Preserve".to_owned()),
        repository.collection(old_id).expect("survivor").title
    );
    let history = repository
        .metadata_history(old_id)
        .expect("all evidence retained");
    assert!(
        history
            .fields
            .iter()
            .find(|field| field.field == MetadataField::Title)
            .expect("title history")
            .assertions
            .iter()
            .any(|assertion| assertion.value_json == "\"candidate external title\"")
    );
}
