use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use doujin_files::{
    DeleteRequest, FileOperationService, ItemStatus, MoveRequest, RecycleBin, RenameRequest,
};
use doujin_parser::PARSER_VERSION;
use doujin_parser::domain::ParseInput;
use doujin_parser::parser::parse_filename;
use doujin_scanner::{FilenameNormalization, PendingCollection, SourceKind};
use doujin_storage::lifecycle::{
    CollectionStatus, DeleteMode, FileOperationStatus, LocationStatus,
};
use doujin_storage::metadata::MetadataField;
use doujin_storage::{CatalogRepository, StorageError};

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
            "doujin-files-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test tree");
        Self { path }
    }

    fn directory(&self, relative: &str) -> PathBuf {
        let path = self.path.join(relative);
        fs::create_dir_all(&path).expect("create directory");
        path
    }

    fn pending(&self, root_name: &str, source: SourceKind, filename: &str) -> PendingCollection {
        let root = self.directory(root_name);
        let path = root.join(filename);
        fs::create_dir_all(path.parent().expect("file parent")).expect("create file parent");
        fs::write(&path, b"zip placeholder").expect("create zip");
        PendingCollection {
            folder: path.parent().expect("folder").to_owned(),
            path,
            root_path: root,
            root_label: root_name.to_owned(),
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
            .is_some_and(|name| name.starts_with("doujin-files-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

struct FakeRecycleBin {
    directory: PathBuf,
    fail_name: Option<String>,
}

impl RecycleBin for FakeRecycleBin {
    fn recycle(&self, path: &Path) -> Result<(), String> {
        let filename = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| "invalid filename".to_owned())?;
        if self.fail_name.as_deref() == Some(filename) {
            return Err("simulated recycle failure".to_owned());
        }
        fs::rename(path, self.directory.join(filename)).map_err(|error| error.to_string())
    }
}

struct DeleteThenErrorRecycleBin;

impl RecycleBin for DeleteThenErrorRecycleBin {
    fn recycle(&self, path: &Path) -> Result<(), String> {
        fs::remove_file(path).map_err(|error| error.to_string())?;
        Err("simulated post-delete error".to_owned())
    }
}

#[test]
fn move_batch_reports_partial_success_without_overwriting() {
    let tree = TestTree::new("move-batch");
    let first = tree.pending("downloads", SourceKind::Downloads, "[circle] first.zip");
    let second = tree.pending("downloads", SourceKind::Downloads, "[circle] second.zip");
    let archive = tree.directory("archive/C106");
    let archive_root = tree.path.join("archive");
    let first_destination = archive.join("[circle] first.zip");
    let second_destination = archive.join("[circle] second.zip");
    fs::write(&second_destination, b"must not overwrite").expect("create conflict");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.ingest_collection(&first).expect("ingest first");
    repository
        .ingest_collection(&second)
        .expect("ingest second");
    let first_id = repository
        .collection_id_for_current_path(&first.path)
        .expect("first lookup")
        .expect("first collection");
    let second_id = repository
        .collection_id_for_current_path(&second.path)
        .expect("second lookup")
        .expect("second collection");
    let archive_root_id = repository
        .register_library_root(&archive_root, SourceKind::Archive, "archive")
        .expect("register archive");
    let recycle = FakeRecycleBin {
        directory: tree.directory("recycle"),
        fail_name: None,
    };

    let report = {
        let mut service = FileOperationService::new(&mut repository, recycle);
        service.move_batch(&[
            MoveRequest {
                collection_id: first_id,
                archive_root_id,
                destination: first_destination.clone(),
            },
            MoveRequest {
                collection_id: second_id,
                archive_root_id,
                destination: second_destination.clone(),
            },
        ])
    };

    assert_eq!(1, report.succeeded());
    assert_eq!(1, report.failed());
    assert_eq!(0, report.pending_recovery());
    assert_eq!(ItemStatus::Succeeded, report.items[0].status);
    assert_eq!(ItemStatus::Failed, report.items[1].status);
    assert!(!first.path.exists());
    assert!(first_destination.exists());
    assert!(second.path.exists());
    assert_eq!(
        b"must not overwrite",
        fs::read(&second_destination)
            .expect("conflict remains")
            .as_slice()
    );
    assert_eq!(
        Some(first_id),
        repository
            .collection_id_for_current_path(&first_destination)
            .expect("moved path")
    );
    assert_eq!(
        Some(second_id),
        repository
            .collection_id_for_current_path(&second.path)
            .expect("failed item path")
    );
    let history = repository
        .location_history(first_id)
        .expect("first history");
    assert_eq!(LocationStatus::Moved, history[0].status);
    assert_eq!(LocationStatus::Current, history[1].status);
}

#[test]
fn delete_batch_supports_soft_hard_and_failed_items_independently() {
    let tree = TestTree::new("delete-batch");
    let soft = tree.pending("downloads", SourceKind::Downloads, "[circle] soft.zip");
    let hard = tree.pending("downloads", SourceKind::Downloads, "[circle] hard.zip");
    let failed = tree.pending("downloads", SourceKind::Downloads, "[circle] failed.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    for pending in [&soft, &hard, &failed] {
        repository
            .ingest_collection(pending)
            .expect("ingest collection");
    }
    let soft_id = repository
        .collection_id_for_current_path(&soft.path)
        .expect("soft lookup")
        .expect("soft collection");
    let hard_id = repository
        .collection_id_for_current_path(&hard.path)
        .expect("hard lookup")
        .expect("hard collection");
    let failed_id = repository
        .collection_id_for_current_path(&failed.path)
        .expect("failed lookup")
        .expect("failed collection");
    let soft_title = repository
        .current_selection(soft_id, MetadataField::Title)
        .expect("soft title")
        .expect("soft title selection");
    let recycle_directory = tree.directory("fake-recycle-bin");
    let recycle = FakeRecycleBin {
        directory: recycle_directory.clone(),
        fail_name: failed
            .path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned()),
    };

    let report = {
        let mut service = FileOperationService::new(&mut repository, recycle);
        service.delete_batch(&[
            DeleteRequest {
                collection_id: soft_id,
                mode: DeleteMode::Soft,
            },
            DeleteRequest {
                collection_id: hard_id,
                mode: DeleteMode::Permanent,
            },
            DeleteRequest {
                collection_id: failed_id,
                mode: DeleteMode::Soft,
            },
        ])
    };

    assert_eq!(2, report.succeeded());
    assert_eq!(1, report.failed());
    assert_eq!(0, report.pending_recovery());
    assert!(!soft.path.exists());
    assert!(recycle_directory.join("[circle] soft.zip").exists());
    assert_eq!(
        CollectionStatus::SoftDeleted,
        repository.collection_status(soft_id).expect("soft status")
    );
    assert_eq!(
        soft_title,
        repository
            .current_selection(soft_id, MetadataField::Title)
            .expect("soft metadata")
            .expect("soft title remains")
    );
    assert!(!hard.path.exists());
    assert!(matches!(
        repository.collection_status(hard_id),
        Err(StorageError::CollectionNotFound(id)) if id == hard_id
    ));
    assert!(failed.path.exists());
    assert_eq!(
        CollectionStatus::Active,
        repository
            .collection_status(failed_id)
            .expect("failed item remains active")
    );
    let failed_operation_id = report.items[2].operation_id.expect("failed operation id");
    let failed_operation = repository
        .file_operation(failed_operation_id)
        .expect("failed operation");
    assert_eq!(FileOperationStatus::Failed, failed_operation.status);
    assert!(failed_operation.error_message.is_some());
    let hard_operation_id = report.items[1].operation_id.expect("hard operation id");
    let hard_operation = repository
        .file_operation(hard_operation_id)
        .expect("hard operation");
    assert_eq!(FileOperationStatus::Succeeded, hard_operation.status);
    assert_eq!(None, hard_operation.collection_id);
}

#[test]
fn ambiguous_post_delete_error_stays_pending_for_recovery() {
    let tree = TestTree::new("pending-recovery");
    let pending = tree.pending("downloads", SourceKind::Downloads, "[circle] uncertain.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .ingest_collection(&pending)
        .expect("ingest collection");
    let collection_id = repository
        .collection_id_for_current_path(&pending.path)
        .expect("path lookup")
        .expect("collection");

    let report = {
        let mut service = FileOperationService::new(&mut repository, DeleteThenErrorRecycleBin);
        service.delete_batch(&[DeleteRequest {
            collection_id,
            mode: DeleteMode::Soft,
        }])
    };

    assert_eq!(1, report.pending_recovery());
    let operation_id = report.items[0].operation_id.expect("operation id");
    assert_eq!(
        FileOperationStatus::Pending,
        repository
            .file_operation(operation_id)
            .expect("pending operation")
            .status
    );
    let recovery = {
        let mut service = FileOperationService::new(&mut repository, DeleteThenErrorRecycleBin);
        service
            .recover_pending()
            .expect("recover pending operations")
    };
    assert_eq!(1, recovery.succeeded());
    assert_eq!(
        CollectionStatus::SoftDeleted,
        repository
            .collection_status(collection_id)
            .expect("reconciled status")
    );
}

#[test]
fn rename_batch_is_same_parent_no_overwrite_and_partial() {
    let tree = TestTree::new("rename-batch");
    let first = tree.pending("archive", SourceKind::Archive, "first.zip");
    let second = tree.pending("archive", SourceKind::Archive, "second.zip");
    let conflict = first.path.with_file_name("occupied.zip");
    fs::write(&conflict, b"occupied").expect("write collision");
    let first_destination = first.path.with_file_name("renamed.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.ingest_collection(&first).expect("ingest first");
    repository
        .ingest_collection(&second)
        .expect("ingest second");
    let first_id = repository
        .collection_id_for_current_path(&first.path)
        .expect("first lookup")
        .expect("first id");
    let second_id = repository
        .collection_id_for_current_path(&second.path)
        .expect("second lookup")
        .expect("second id");
    repository
        .add_collection_tag(first_id, "保留標籤")
        .expect("tag first");
    let before = repository.collection(first_id).expect("before snapshot");

    let report = {
        let mut service = FileOperationService::new(
            &mut repository,
            FakeRecycleBin {
                directory: tree.directory("recycle"),
                fail_name: None,
            },
        );
        service.rename_batch(&[
            RenameRequest {
                collection_id: first_id,
                expected_source: first.path.clone(),
                destination: first_destination.clone(),
            },
            RenameRequest {
                collection_id: second_id,
                expected_source: second.path.clone(),
                destination: conflict.clone(),
            },
        ])
    };

    assert_eq!(1, report.succeeded());
    assert_eq!(1, report.failed());
    assert!(!first.path.exists());
    assert!(first_destination.exists());
    assert!(second.path.exists());
    assert_eq!(
        b"occupied".as_slice(),
        fs::read(&conflict).expect("collision intact").as_slice()
    );
    assert_eq!(
        Some(first_id),
        repository
            .collection_id_for_current_path(&first_destination)
            .expect("new path")
    );
    let after = repository.collection(first_id).expect("after snapshot");
    assert_eq!(before.id, after.id);
    assert_eq!(before.title, after.title);
    assert_eq!(before.event, after.event);
    assert_eq!(before.circle, after.circle);
    assert_eq!(before.authors, after.authors);
    assert_eq!(before.tags, after.tags);
    assert_eq!(first_destination, after.path);
    assert_eq!(
        2,
        repository
            .location_history(first_id)
            .expect("history")
            .len()
    );
}

#[test]
fn rename_rejects_toctou_and_cross_parent_without_journaling() {
    let tree = TestTree::new("rename-safety");
    let pending = tree.pending("archive", SourceKind::Archive, "source.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.ingest_collection(&pending).expect("ingest");
    let collection_id = repository
        .collection_id_for_current_path(&pending.path)
        .expect("lookup")
        .expect("id");
    let different_source = pending.path.with_file_name("stale.zip");
    let cross_parent = tree.directory("elsewhere").join("renamed.zip");
    let before_count = repository.file_operation_count().expect("operation count");

    assert!(
        repository
            .begin_rename(
                collection_id,
                &different_source,
                &pending.path.with_file_name("renamed.zip")
            )
            .expect_err("source changed")
            .to_string()
            .contains("來源已變更")
    );
    assert!(
        repository
            .begin_rename(collection_id, &pending.path, &cross_parent)
            .expect_err("cross parent")
            .to_string()
            .contains("相同 parent")
    );
    assert_eq!(
        before_count,
        repository.file_operation_count().expect("unchanged count")
    );
}

#[test]
fn interrupted_rename_uses_existing_pending_recovery() {
    let tree = TestTree::new("rename-recovery");
    let pending = tree.pending("downloads", SourceKind::Downloads, "source.zip");
    let destination = pending.path.with_file_name("renamed.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.ingest_collection(&pending).expect("ingest");
    let collection_id = repository
        .collection_id_for_current_path(&pending.path)
        .expect("lookup")
        .expect("id");
    let operation = repository
        .begin_rename(collection_id, &pending.path, &destination)
        .expect("begin rename");
    fs::rename(&pending.path, &destination).expect("simulate applied filesystem rename");

    let recovery = {
        let mut service = FileOperationService::new(
            &mut repository,
            FakeRecycleBin {
                directory: tree.directory("recycle"),
                fail_name: None,
            },
        );
        service.recover_pending().expect("recover pending")
    };

    assert_eq!(1, recovery.succeeded());
    assert_eq!(Some(operation.id), recovery.items[0].operation_id);
    assert_eq!(
        FileOperationStatus::Succeeded,
        repository
            .file_operation(operation.id)
            .expect("operation")
            .status
    );
    assert_eq!(
        Some(collection_id),
        repository
            .collection_id_for_current_path(&destination)
            .expect("recovered current path")
    );
}
