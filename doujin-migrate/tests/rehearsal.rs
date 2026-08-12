use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use doujin_migrate::{MigrationError, MigrationStatus, run_migration};
use doujin_storage::CatalogRepository;
use doujin_storage::metadata::{
    ConfidenceEvidence, ExternalCandidate, ExternalCandidateOutcome, MetadataField, MetadataSource,
    MetadataValue,
};
use rusqlite::{Connection, params};

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
            "doujin-migrate-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test tree");
        Self { path }
    }

    fn source(&self) -> PathBuf {
        self.path.join("legacy.db")
    }

    fn target(&self) -> PathBuf {
        self.path.join("v2.db")
    }
}

impl Drop for TestTree {
    fn drop(&mut self) {
        if self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("doujin-migrate-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[test]
fn rehearsal_preserves_source_and_validates_mapped_metadata_and_tags() {
    let tree = TestTree::new("happy");
    create_fixture(&tree.source(), false);
    let source_before = fs::read(tree.source()).expect("source before");
    assert_source_has_no_sidecars(&tree.source());

    let report = run_migration(tree.source(), tree.target()).expect("migration rehearsal");

    assert_eq!(MigrationStatus::Completed, report.status);
    assert!(report.passed());
    assert!(report.source_fingerprint.unchanged);
    assert_eq!(
        source_before,
        fs::read(tree.source()).expect("source after")
    );
    assert_source_has_no_sidecars(&tree.source());
    assert_eq!(2, report.source_counts.collections);
    assert_eq!(2, report.source_counts.zip_collections);
    assert_eq!(0, report.source_counts.image_folders);
    assert_eq!(2, report.source_counts.tags);
    assert_eq!(2, report.source_counts.tag_links);
    assert_eq!(
        2,
        report
            .target_counts
            .as_ref()
            .expect("target counts")
            .collections
    );
    assert_eq!(
        2,
        report
            .target_counts
            .as_ref()
            .expect("target counts")
            .zip_collections
    );
    assert_eq!(
        0,
        report
            .target_counts
            .as_ref()
            .expect("target counts")
            .image_folders
    );
    assert_eq!(2, report.sample_metadata.checked);
    assert!(report.sample_metadata.mismatches.is_empty());
    assert!(report.path_conflicts.is_empty());
    assert!(report.blocking_issues.is_empty());
    assert_eq!(
        Some(&"viewer.exe".to_owned()),
        report.reapply_setting_values.get("viewer_path")
    );
    assert_eq!(
        Some(1),
        report
            .effective_empty_value_comparison
            .get("event")
            .expect("event empty comparison")
            .target
    );

    let target = Connection::open(tree.target()).expect("open target");
    let classification = target
        .query_row(
            "SELECT classification_top, classification_subcategory
             FROM effective_metadata WHERE collection_id = 7",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("classification");
    assert_eq!(
        ("商業誌".to_owned(), "成年コミック".to_owned()),
        classification
    );
    let protected_legacy_selections: i64 = target
        .query_row(
            "SELECT count(*)
             FROM metadata_selections AS selection
             JOIN metadata_assertions AS assertion ON assertion.id = selection.assertion_id
             WHERE selection.selected_by = 'migration' AND assertion.source_kind = 'legacy'",
            [],
            |row| row.get(0),
        )
        .expect("legacy selections");
    assert!(protected_legacy_selections > 0);
    drop(target);

    let mut repository = CatalogRepository::open(tree.target()).expect("open v2 repository");
    let outcome = repository
        .save_external_candidate(ExternalCandidate {
            collection_id: 7,
            field: MetadataField::Title,
            value: MetadataValue::Text("外部標題".to_owned()),
            source_reference: "fixture:external".to_owned(),
            confidence: ConfidenceEvidence {
                total: 0.99,
                source_reliability: 0.99,
                identifier_match: 1.0,
                string_similarity: 0.99,
                rule_certainty: 0.99,
                reliable_identifier_exact_match: true,
                reason: "migration protection test".to_owned(),
            },
        })
        .expect("save external candidate");
    assert!(matches!(
        outcome,
        ExternalCandidateOutcome::Suggestion { .. }
    ));
    let selected = repository
        .current_selection(7, MetadataField::Title)
        .expect("legacy title selection")
        .expect("selected title");
    assert_eq!(MetadataSource::Legacy, selected.source);
}

#[test]
fn rehearsal_preserves_legacy_image_folder_media_kind() {
    let tree = TestTree::new("image-folder");
    create_fixture(&tree.source(), false);
    let connection = Connection::open(tree.source()).expect("open legacy fixture");
    connection
        .execute(
            "INSERT INTO doujinshi(
                 id, filename, filepath, folder, event, circle, author, title, parody,
                 is_dl, created_at, updated_at, category, source
             ) VALUES (11, 'Image Folder', 'X:\\archive\\C2\\Image Folder', 'C2', NULL,
                       'Circle', NULL, '圖片資料夾作品', NULL, 0,
                       '2022-01-01 00:00:00', '2022-01-01 00:00:00', '同人誌', 'archive')",
            [],
        )
        .expect("image folder collection");
    drop(connection);
    assert_source_has_no_sidecars(&tree.source());

    let report = run_migration(tree.source(), tree.target()).expect("migration rehearsal");

    assert_eq!(MigrationStatus::Completed, report.status);
    assert_eq!(1, report.source_counts.image_folders);
    assert_eq!(
        1,
        report.target_counts.expect("target counts").image_folders
    );
    let target = Connection::open(tree.target()).expect("open target");
    let media_kind: String = target
        .query_row(
            "SELECT media_kind FROM collections WHERE id = 11",
            [],
            |row| row.get(0),
        )
        .expect("image folder media kind");
    assert_eq!("image_folder", media_kind);
}

#[test]
fn normalized_path_conflict_blocks_before_target_creation() {
    let tree = TestTree::new("conflict");
    create_fixture(&tree.source(), true);
    let source_before = fs::read(tree.source()).expect("source before");
    assert_source_has_no_sidecars(&tree.source());

    let report = run_migration(tree.source(), tree.target()).expect("blocked report");

    assert_eq!(MigrationStatus::Blocked, report.status);
    assert_eq!(1, report.path_conflicts.len());
    assert!(!tree.target().exists());
    assert!(report.source_fingerprint.unchanged);
    assert_eq!(
        source_before,
        fs::read(tree.source()).expect("source after")
    );
    assert_source_has_no_sidecars(&tree.source());
}

#[test]
fn existing_target_is_never_overwritten() {
    let tree = TestTree::new("existing-target");
    create_fixture(&tree.source(), false);
    fs::write(tree.target(), b"owner data").expect("create existing target");

    let error = run_migration(tree.source(), tree.target()).expect_err("must refuse target");

    assert!(matches!(error, MigrationError::TargetAlreadyExists(_)));
    assert_eq!(
        b"owner data".to_vec(),
        fs::read(tree.target()).expect("target unchanged")
    );
}

#[test]
fn source_with_wal_sidecar_is_rejected_before_open() {
    let tree = TestTree::new("source-sidecar");
    create_fixture(&tree.source(), false);
    let wal = appended_path(&tree.source(), "-wal");
    fs::write(&wal, b"pending WAL marker").expect("source sidecar");

    let error = run_migration(tree.source(), tree.target()).expect_err("must refuse WAL source");

    assert!(matches!(error, MigrationError::SourceHasSidecars(_)));
    assert!(!tree.target().exists());
}

fn create_fixture(path: &Path, conflict: bool) {
    let connection = Connection::open(path).expect("create legacy fixture");
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .expect("legacy WAL mode");
    connection
        .execute_batch(
            "CREATE TABLE doujinshi (
                 id INTEGER PRIMARY KEY,
                 filename TEXT,
                 filepath TEXT,
                 folder TEXT,
                 event TEXT,
                 circle TEXT,
                 author TEXT,
                 title TEXT,
                 parody TEXT,
                 is_dl INTEGER,
                 created_at TEXT,
                 updated_at TEXT,
                 category TEXT,
                 source TEXT
             );
             CREATE TABLE tags (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE doujinshi_tags (
                 doujinshi_id INTEGER NOT NULL,
                 tag_id INTEGER NOT NULL,
                 PRIMARY KEY (doujinshi_id, tag_id)
             );
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);",
        )
        .expect("legacy schema");
    let roots = serde_json::json!([
        {"path": "X:\\archive", "source": "archive", "label": "歸檔區"},
        {"path": "H:\\", "source": "downloads", "label": "下載區"}
    ])
    .to_string();
    connection
        .execute(
            "INSERT INTO settings(key, value) VALUES ('scan_roots', ?1)",
            [roots],
        )
        .expect("scan roots");
    connection
        .execute(
            "INSERT INTO settings(key, value) VALUES ('viewer_path', 'viewer.exe')",
            [],
        )
        .expect("ignored setting");
    connection
        .execute(
            "INSERT INTO doujinshi(
                 id, filename, filepath, folder, event, circle, author, title, parody,
                 is_dl, created_at, updated_at, category, source
             ) VALUES (7, 'Book.zip', 'X:\\archive\\C1\\Book.zip', 'C1', NULL,
                       NULL, '作者A, 作者B', '作品', '原創', 1,
                       '2020-01-01 00:00:00', '2020-01-02 00:00:00',
                       '成年コミック', 'archive')",
            [],
        )
        .expect("first collection");
    let second_path = if conflict {
        "x:/ARCHIVE/c1/Book.zip"
    } else {
        "H:\\Download.zip"
    };
    let second_filename = if conflict { "Book.zip" } else { "Download.zip" };
    let second_source = if conflict { "archive" } else { "downloads" };
    connection
        .execute(
            "INSERT INTO doujinshi(
                 id, filename, filepath, folder, event, circle, author, title, parody,
                 is_dl, created_at, updated_at, category, source
             ) VALUES (9, ?1, ?2, '', 'C100', 'Circle', NULL, '下載作品', NULL, 0,
                       '2021-01-01 00:00:00', '2021-01-01 00:00:00', '同人誌', ?3)",
            params![second_filename, second_path, second_source],
        )
        .expect("second collection");
    connection
        .execute_batch(
            "INSERT INTO tags(id, name) VALUES (3, 'favorite'), (4, 'review');
             INSERT INTO doujinshi_tags(doujinshi_id, tag_id) VALUES (7, 3), (9, 4);",
        )
        .expect("legacy tags");
}

fn assert_source_has_no_sidecars(source: &Path) {
    for suffix in ["-wal", "-shm"] {
        let mut path = source.as_os_str().to_os_string();
        path.push(suffix);
        assert!(!PathBuf::from(path).exists(), "unexpected source {suffix}");
    }
}

fn appended_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}
