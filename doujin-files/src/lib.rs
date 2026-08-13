use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use doujin_scanner::SourceKind;
use doujin_storage::lifecycle::{DeleteMode, FileOperationKind, PendingFileOperation};
use doujin_storage::{CatalogRepository, StorageError};

static PARTIAL_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub enum FileServiceError {
    Storage(StorageError),
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    RecycleBin(String),
    InvalidFile(String),
}

impl fmt::Display for FileServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(formatter, "{error}"),
            Self::Io {
                action,
                path,
                source,
            } => write!(formatter, "{action}失敗：{}：{source}", path.display()),
            Self::RecycleBin(error) => write!(formatter, "送往資源回收桶失敗：{error}"),
            Self::InvalidFile(reason) => write!(formatter, "檔案操作無效：{reason}"),
        }
    }
}

impl Error for FileServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::RecycleBin(_) | Self::InvalidFile(_) => None,
        }
    }
}

impl From<StorageError> for FileServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

pub trait RecycleBin {
    fn recycle(&self, path: &Path) -> Result<(), String>;
}

impl<R: RecycleBin + ?Sized> RecycleBin for &R {
    fn recycle(&self, path: &Path) -> Result<(), String> {
        (*self).recycle(path)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRecycleBin;

impl RecycleBin for SystemRecycleBin {
    fn recycle(&self, path: &Path) -> Result<(), String> {
        trash::delete(path).map_err(|error| error.to_string())
    }
}

pub trait CollectionLauncher: Send {
    fn open_default(&self, path: &Path) -> io::Result<()>;
    fn open_with_reader(&self, reader: &Path, path: &Path) -> io::Result<()>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCollectionLauncher;

impl CollectionLauncher for SystemCollectionLauncher {
    fn open_default(&self, path: &Path) -> io::Result<()> {
        open::that_detached(path)
    }

    fn open_with_reader(&self, reader: &Path, path: &Path) -> io::Result<()> {
        let reader = reader.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "閱讀器路徑不是有效的 Unicode")
        })?;
        open::with_detached(path, reader.to_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchAction {
    SystemDefault,
    ConfiguredReader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchReceipt {
    pub collection_id: i64,
    pub action: LaunchAction,
}

#[derive(Debug)]
pub enum LaunchError {
    Storage(StorageError),
    CollectionFileNotFound,
    InvalidCollectionFile(String),
    ReaderNotConfigured,
    ReaderUnavailable,
    Launcher {
        action: LaunchAction,
        source: io::Error,
    },
}

impl fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
            Self::CollectionFileNotFound => write!(formatter, "收藏檔案不存在"),
            Self::InvalidCollectionFile(reason) => {
                write!(formatter, "收藏檔案不能安全開啟：{reason}")
            }
            Self::ReaderNotConfigured => write!(formatter, "尚未設定閱讀器"),
            Self::ReaderUnavailable => write!(formatter, "設定的閱讀器不存在或不是一般檔案"),
            Self::Launcher { action, source } => {
                write!(
                    formatter,
                    "{}啟動失敗：{source}",
                    launch_action_name(*action)
                )
            }
        }
    }
}

impl Error for LaunchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Launcher { source, .. } => Some(source),
            Self::CollectionFileNotFound
            | Self::InvalidCollectionFile(_)
            | Self::ReaderNotConfigured
            | Self::ReaderUnavailable => None,
        }
    }
}

impl From<StorageError> for LaunchError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

pub struct CollectionLaunchService<'repository, 'launcher> {
    repository: &'repository CatalogRepository,
    launcher: &'launcher dyn CollectionLauncher,
}

impl<'repository, 'launcher> CollectionLaunchService<'repository, 'launcher> {
    pub fn new(
        repository: &'repository CatalogRepository,
        launcher: &'launcher dyn CollectionLauncher,
    ) -> Self {
        Self {
            repository,
            launcher,
        }
    }

    pub fn open_default(&self, collection_id: i64) -> Result<LaunchReceipt, LaunchError> {
        self.launch(collection_id, LaunchAction::SystemDefault, None)
    }

    pub fn open_with_reader(
        &self,
        collection_id: i64,
        reader: Option<&Path>,
    ) -> Result<LaunchReceipt, LaunchError> {
        let reader = reader.ok_or(LaunchError::ReaderNotConfigured)?;
        if !reader.is_absolute() {
            return Err(LaunchError::ReaderUnavailable);
        }
        let metadata = fs::symlink_metadata(reader).map_err(|_| LaunchError::ReaderUnavailable)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(LaunchError::ReaderUnavailable);
        }
        self.launch(collection_id, LaunchAction::ConfiguredReader, Some(reader))
    }

    fn launch(
        &self,
        collection_id: i64,
        action: LaunchAction,
        reader: Option<&Path>,
    ) -> Result<LaunchReceipt, LaunchError> {
        let path = self.repository.active_collection_file_path(collection_id)?;
        validate_launchable_zip(&path)?;
        let result = match reader {
            Some(reader) => self.launcher.open_with_reader(reader, &path),
            None => self.launcher.open_default(&path),
        };
        result.map_err(|source| LaunchError::Launcher { action, source })?;
        Ok(LaunchReceipt {
            collection_id,
            action,
        })
    }
}

fn launch_action_name(action: LaunchAction) -> &'static str {
    match action {
        LaunchAction::SystemDefault => "系統預設程式",
        LaunchAction::ConfiguredReader => "指定閱讀器",
    }
}

fn validate_launchable_zip(path: &Path) -> Result<(), LaunchError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(LaunchError::CollectionFileNotFound);
        }
        Err(error) => return Err(LaunchError::InvalidCollectionFile(error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LaunchError::InvalidCollectionFile(
            "路徑必須是一般檔案且不能是 symlink".to_owned(),
        ));
    }
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        return Err(LaunchError::InvalidCollectionFile(
            "只允許開啟 ZIP 收藏".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRequest {
    pub collection_id: i64,
    pub archive_root_id: i64,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameRequest {
    pub collection_id: i64,
    pub expected_source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteRequest {
    pub collection_id: i64,
    pub mode: DeleteMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemStatus {
    Succeeded,
    Failed,
    PendingRecovery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemResult {
    pub collection_id: i64,
    pub operation_id: Option<i64>,
    pub status: ItemStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchReport {
    pub items: Vec<ItemResult>,
}

impl BatchReport {
    pub fn succeeded(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == ItemStatus::Succeeded)
            .count()
    }

    pub fn failed(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == ItemStatus::Failed)
            .count()
    }

    pub fn pending_recovery(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == ItemStatus::PendingRecovery)
            .count()
    }
}

pub struct FileOperationService<'repository, R> {
    repository: &'repository mut CatalogRepository,
    recycle_bin: R,
}

impl<'repository, R: RecycleBin> FileOperationService<'repository, R> {
    pub fn new(repository: &'repository mut CatalogRepository, recycle_bin: R) -> Self {
        Self {
            repository,
            recycle_bin,
        }
    }

    pub fn move_batch(&mut self, requests: &[MoveRequest]) -> BatchReport {
        BatchReport {
            items: requests
                .iter()
                .map(|request| self.move_one(request))
                .collect(),
        }
    }

    pub fn rename_batch(&mut self, requests: &[RenameRequest]) -> BatchReport {
        BatchReport {
            items: requests
                .iter()
                .map(|request| self.rename_one(request))
                .collect(),
        }
    }

    pub fn move_to_archive_batch(
        &mut self,
        collection_ids: &[i64],
        archive_root_id: i64,
    ) -> BatchReport {
        BatchReport {
            items: collection_ids
                .iter()
                .map(|collection_id| {
                    let request = match self.archive_move_request(*collection_id, archive_root_id) {
                        Ok(request) => request,
                        Err(error) => {
                            return failed_without_operation(*collection_id, error.to_string());
                        }
                    };
                    self.move_one(&request)
                })
                .collect(),
        }
    }

    pub fn delete_batch(&mut self, requests: &[DeleteRequest]) -> BatchReport {
        BatchReport {
            items: requests
                .iter()
                .map(|request| self.delete_one(*request))
                .collect(),
        }
    }

    pub fn recover_pending(&mut self) -> Result<BatchReport, FileServiceError> {
        let operations = self.repository.pending_file_operations()?;
        Ok(BatchReport {
            items: operations
                .iter()
                .map(|operation| self.recover_one(operation))
                .collect(),
        })
    }

    fn archive_move_request(
        &self,
        collection_id: i64,
        archive_root_id: i64,
    ) -> Result<MoveRequest, FileServiceError> {
        let collection = self.repository.collection(collection_id)?;
        if !collection
            .root
            .as_ref()
            .is_some_and(|root| root.source == SourceKind::Downloads)
        {
            return Err(FileServiceError::InvalidFile(
                "系統 move 只能從下載區開始".to_owned(),
            ));
        }
        validate_zip_filename_component(&collection.filename)?;
        validate_source_zip(&collection.path)?;

        let archive_root = self.repository.library_root(archive_root_id)?;
        if archive_root.source != SourceKind::Archive || !archive_root.active {
            return Err(FileServiceError::InvalidFile(
                "move 目標必須是啟用中的歸檔區".to_owned(),
            ));
        }
        let event_directory = archive_root
            .path
            .join(safe_archive_folder(collection.event.as_deref()));
        ensure_safe_archive_directory(&archive_root.path, &event_directory)?;

        Ok(MoveRequest {
            collection_id,
            archive_root_id,
            destination: event_directory.join(collection.filename),
        })
    }

    fn move_one(&mut self, request: &MoveRequest) -> ItemResult {
        let operation = match self.repository.begin_system_move(
            request.collection_id,
            request.archive_root_id,
            &request.destination,
        ) {
            Ok(operation) => operation,
            Err(error) => {
                return failed_without_operation(request.collection_id, error.to_string());
            }
        };
        match move_zip_no_overwrite(&operation.from_path, &request.destination) {
            Ok(()) => match self.repository.complete_file_operation(operation.id) {
                Ok(()) => succeeded(&operation),
                Err(error) => pending_recovery(&operation, error.to_string()),
            },
            Err(error) => self.handle_filesystem_failure(&operation, error.to_string()),
        }
    }

    fn rename_one(&mut self, request: &RenameRequest) -> ItemResult {
        if let Err(error) = validate_rename_destination(&request.destination) {
            return failed_without_operation(request.collection_id, error.to_string());
        }
        let operation = match self.repository.begin_rename(
            request.collection_id,
            &request.expected_source,
            &request.destination,
        ) {
            Ok(operation) => operation,
            Err(error) => {
                return failed_without_operation(request.collection_id, error.to_string());
            }
        };
        match move_zip_no_overwrite(&operation.from_path, &request.destination) {
            Ok(()) => match self.repository.complete_file_operation(operation.id) {
                Ok(()) => succeeded(&operation),
                Err(error) => pending_recovery(&operation, error.to_string()),
            },
            Err(error) => self.handle_filesystem_failure(&operation, error.to_string()),
        }
    }

    fn delete_one(&mut self, request: DeleteRequest) -> ItemResult {
        let operation = match self
            .repository
            .begin_delete(request.collection_id, request.mode)
        {
            Ok(operation) => operation,
            Err(error) => {
                return failed_without_operation(request.collection_id, error.to_string());
            }
        };
        let file_result =
            validate_source_zip(&operation.from_path).and_then(|()| match request.mode {
                DeleteMode::Soft => self
                    .recycle_bin
                    .recycle(&operation.from_path)
                    .map_err(FileServiceError::RecycleBin),
                DeleteMode::Permanent => {
                    fs::remove_file(&operation.from_path).map_err(|source| FileServiceError::Io {
                        action: "永久刪除",
                        path: operation.from_path.clone(),
                        source,
                    })
                }
            });
        match file_result {
            Ok(()) => match self.repository.complete_file_operation(operation.id) {
                Ok(()) => succeeded(&operation),
                Err(error) => pending_recovery(&operation, error.to_string()),
            },
            Err(error) => self.handle_filesystem_failure(&operation, error.to_string()),
        }
    }

    fn handle_filesystem_failure(
        &mut self,
        operation: &PendingFileOperation,
        error: String,
    ) -> ItemResult {
        let destination_exists = operation.to_path.as_ref().is_some_and(|path| path.exists());
        if !operation.from_path.exists() || destination_exists {
            return pending_recovery(operation, error);
        }
        match self.repository.fail_file_operation(operation.id, &error) {
            Ok(()) => failed(operation, error),
            Err(storage_error) => pending_recovery(
                operation,
                format!("{error}；另無法記錄 failed：{storage_error}"),
            ),
        }
    }

    fn recover_one(&mut self, operation: &PendingFileOperation) -> ItemResult {
        let source_exists = operation.from_path.exists();
        let destination_exists = operation.to_path.as_ref().is_some_and(|path| path.exists());
        let can_complete = match operation.kind {
            FileOperationKind::Rename | FileOperationKind::Move => {
                !source_exists && destination_exists
            }
            FileOperationKind::SoftDelete | FileOperationKind::HardDelete => !source_exists,
        };
        if can_complete {
            return match self.repository.complete_file_operation(operation.id) {
                Ok(()) => succeeded(operation),
                Err(error) => pending_recovery(operation, error.to_string()),
            };
        }
        let definitely_not_applied = match operation.kind {
            FileOperationKind::Rename | FileOperationKind::Move => {
                source_exists && !destination_exists
            }
            FileOperationKind::SoftDelete | FileOperationKind::HardDelete => source_exists,
        };
        if definitely_not_applied {
            let error = "程式重啟後確認檔案操作尚未套用".to_owned();
            return match self.repository.fail_file_operation(operation.id, &error) {
                Ok(()) => failed(operation, error),
                Err(storage_error) => pending_recovery(operation, storage_error.to_string()),
            };
        }
        pending_recovery(operation, "來源與目標狀態不明確，需要人工復原".to_owned())
    }
}

fn succeeded(operation: &PendingFileOperation) -> ItemResult {
    ItemResult {
        collection_id: operation.collection_id,
        operation_id: Some(operation.id),
        status: ItemStatus::Succeeded,
        error: None,
    }
}

fn failed(operation: &PendingFileOperation, error: String) -> ItemResult {
    ItemResult {
        collection_id: operation.collection_id,
        operation_id: Some(operation.id),
        status: ItemStatus::Failed,
        error: Some(error),
    }
}

fn failed_without_operation(collection_id: i64, error: String) -> ItemResult {
    ItemResult {
        collection_id,
        operation_id: None,
        status: ItemStatus::Failed,
        error: Some(error),
    }
}

fn pending_recovery(operation: &PendingFileOperation, error: String) -> ItemResult {
    ItemResult {
        collection_id: operation.collection_id,
        operation_id: Some(operation.id),
        status: ItemStatus::PendingRecovery,
        error: Some(error),
    }
}

fn move_zip_no_overwrite(source: &Path, destination: &Path) -> Result<(), FileServiceError> {
    move_zip_no_overwrite_with_strategy(source, destination, false)
}

fn safe_archive_folder(event: Option<&str>) -> String {
    let event = event.unwrap_or_default().trim();
    let mut folder: String = event
        .chars()
        .map(|character| {
            if character.is_control() || r#"<>:"/\|?*"#.contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    folder = folder.trim().trim_end_matches([' ', '.']).to_owned();
    if folder.is_empty() || matches!(folder.as_str(), "." | "..") {
        return "未分類".to_owned();
    }
    if is_windows_reserved_name(&folder) {
        folder.insert(0, '_');
    }
    folder
}

fn is_windows_reserved_name(value: &str) -> bool {
    let basename = value.split('.').next().unwrap_or_default();
    let uppercase = basename.to_ascii_uppercase();
    matches!(uppercase.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (uppercase.len() == 4
            && (uppercase.starts_with("COM") || uppercase.starts_with("LPT"))
            && matches!(uppercase.as_bytes()[3], b'1'..=b'9'))
}

fn validate_zip_filename_component(filename: &str) -> Result<(), FileServiceError> {
    if filename.is_empty()
        || filename.ends_with([' ', '.'])
        || filename
            .chars()
            .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
        || !filename
            .to_ascii_lowercase()
            .strip_suffix(".zip")
            .is_some_and(|basename| !basename.is_empty() && !is_windows_reserved_name(basename))
    {
        return Err(FileServiceError::InvalidFile(
            "收藏檔名不是安全的 ZIP filename component".to_owned(),
        ));
    }
    Ok(())
}

fn validate_rename_destination(destination: &Path) -> Result<(), FileServiceError> {
    let filename = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| FileServiceError::InvalidFile("rename 目標檔名不是 Unicode".to_owned()))?;
    validate_zip_filename_component(filename)?;
    if filename.encode_utf16().count() > 255 {
        return Err(FileServiceError::InvalidFile(
            "rename 目標檔名超過 Windows component 長度限制".to_owned(),
        ));
    }
    if destination.to_string_lossy().encode_utf16().count() > 259 {
        return Err(FileServiceError::InvalidFile(
            "rename 目標超過 Windows 相容 path 長度限制".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_safe_archive_directory(root: &Path, directory: &Path) -> Result<(), FileServiceError> {
    let canonical_root = fs::canonicalize(root).map_err(|source| FileServiceError::Io {
        action: "解析歸檔區",
        path: root.to_owned(),
        source,
    })?;
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(FileServiceError::InvalidFile(format!(
                "歸檔場次路徑必須是一般資料夾且不能是 symlink：{}",
                directory.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(directory).map_err(|source| FileServiceError::Io {
                action: "建立歸檔場次資料夾",
                path: directory.to_owned(),
                source,
            })?;
        }
        Err(source) => {
            return Err(FileServiceError::Io {
                action: "檢查歸檔場次資料夾",
                path: directory.to_owned(),
                source,
            });
        }
    }
    let canonical_directory =
        fs::canonicalize(directory).map_err(|source| FileServiceError::Io {
            action: "解析歸檔場次資料夾",
            path: directory.to_owned(),
            source,
        })?;
    if !canonical_directory.starts_with(&canonical_root) {
        return Err(FileServiceError::InvalidFile(format!(
            "歸檔場次資料夾不在指定歸檔區內：{}",
            directory.display()
        )));
    }
    Ok(())
}

fn move_zip_no_overwrite_with_strategy(
    source: &Path,
    destination: &Path,
    force_copy: bool,
) -> Result<(), FileServiceError> {
    validate_source_zip(source)?;
    if destination.exists() {
        return Err(FileServiceError::InvalidFile(format!(
            "目標已存在，禁止覆寫：{}",
            destination.display()
        )));
    }

    if !force_copy {
        match fs::hard_link(source, destination) {
            Ok(()) => return remove_source_or_rollback(source, destination),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(FileServiceError::Io {
                    action: "建立 move 目標",
                    path: destination.to_owned(),
                    source: error,
                });
            }
            Err(_) => {}
        }
    }
    copy_publish_then_remove(source, destination)
}

fn validate_source_zip(source: &Path) -> Result<(), FileServiceError> {
    let metadata = fs::symlink_metadata(source).map_err(|source_error| FileServiceError::Io {
        action: "讀取來源",
        path: source.to_owned(),
        source: source_error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(FileServiceError::InvalidFile(format!(
            "來源必須是一般檔案且不能是 symlink：{}",
            source.display()
        )));
    }
    if !source
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        return Err(FileServiceError::InvalidFile(
            "來源必須是 ZIP 檔案".to_owned(),
        ));
    }
    Ok(())
}

fn remove_source_or_rollback(source: &Path, destination: &Path) -> Result<(), FileServiceError> {
    if let Err(remove_error) = fs::remove_file(source) {
        return match fs::remove_file(destination) {
            Ok(()) => Err(FileServiceError::Io {
                action: "移除 move 來源",
                path: source.to_owned(),
                source: remove_error,
            }),
            Err(rollback_error) => Err(FileServiceError::InvalidFile(format!(
                "來源移除失敗且目標 rollback 失敗；需要人工處理：{}；{}",
                remove_error, rollback_error
            ))),
        };
    }
    Ok(())
}

fn copy_publish_then_remove(source: &Path, destination: &Path) -> Result<(), FileServiceError> {
    let mut source_file = File::open(source).map_err(|source_error| FileServiceError::Io {
        action: "開啟 move 來源",
        path: source.to_owned(),
        source: source_error,
    })?;
    let source_size = source_file
        .metadata()
        .map_err(|source_error| FileServiceError::Io {
            action: "讀取來源 metadata",
            path: source.to_owned(),
            source: source_error,
        })?
        .len();
    let (partial_path, mut partial_file) = create_partial_file(destination)?;
    let partial_guard = PartialFile::new(partial_path.clone());
    let (copied, source_digest) =
        copy_and_digest(&mut source_file, &mut partial_file).map_err(|source_error| {
            FileServiceError::Io {
                action: "複製 move 來源",
                path: partial_path.clone(),
                source: source_error,
            }
        })?;
    partial_file
        .sync_all()
        .map_err(|source_error| FileServiceError::Io {
            action: "同步 move 暫存檔",
            path: partial_path.clone(),
            source: source_error,
        })?;
    drop(partial_file);
    let partial_size = fs::metadata(&partial_path)
        .map_err(|source_error| FileServiceError::Io {
            action: "驗證 move 暫存檔",
            path: partial_path.clone(),
            source: source_error,
        })?
        .len();
    if copied != source_size || partial_size != source_size {
        return Err(FileServiceError::InvalidFile(format!(
            "move 複製大小不一致：來源 {source_size}，copied {copied}，暫存 {partial_size}"
        )));
    }
    let partial_digest = digest_file(&partial_path)?;
    if source_digest != partial_digest {
        return Err(FileServiceError::InvalidFile(format!(
            "move 複製 digest 不一致：來源 {source_digest}，暫存 {partial_digest}"
        )));
    }
    fs::hard_link(&partial_path, destination).map_err(|source_error| FileServiceError::Io {
        action: "發布 move 目標",
        path: destination.to_owned(),
        source: source_error,
    })?;
    fs::remove_file(&partial_path).map_err(|source_error| FileServiceError::Io {
        action: "移除 move 暫存檔",
        path: partial_path.clone(),
        source: source_error,
    })?;
    partial_guard.disarm();
    remove_source_or_rollback(source, destination)
}

fn copy_and_digest(source: &mut File, destination: &mut File) -> io::Result<(u64, blake3::Hash)> {
    let mut buffer = [0_u8; 128 * 1024];
    let mut copied = 0_u64;
    let mut hasher = blake3::Hasher::new();
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        destination.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        copied += read as u64;
    }
    Ok((copied, hasher.finalize()))
}

fn digest_file(path: &Path) -> Result<blake3::Hash, FileServiceError> {
    let file = File::open(path).map_err(|source_error| FileServiceError::Io {
        action: "開啟 digest 驗證檔",
        path: path.to_owned(),
        source: source_error,
    })?;
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_reader(file)
        .map_err(|source_error| FileServiceError::Io {
            action: "計算 move 暫存檔 digest",
            path: path.to_owned(),
            source: source_error,
        })?;
    Ok(hasher.finalize())
}

fn create_partial_file(destination: &Path) -> Result<(PathBuf, File), FileServiceError> {
    let parent = destination.parent().ok_or_else(|| {
        FileServiceError::InvalidFile("move 目標缺少 parent directory".to_owned())
    })?;
    for _ in 0..100 {
        let counter = PARTIAL_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".doujin-tagger-{}-{counter}.partial",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source_error) => {
                return Err(FileServiceError::Io {
                    action: "建立 move 暫存檔",
                    path,
                    source: source_error,
                });
            }
        }
    }
    Err(FileServiceError::InvalidFile(
        "無法配置唯一的 move 暫存檔名".to_owned(),
    ))
}

struct PartialFile {
    path: PathBuf,
    armed: std::cell::Cell<bool>,
}

impl PartialFile {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            armed: std::cell::Cell::new(true),
        }
    }

    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if self.armed.get() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "doujin-files-unit-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test directory");
        path
    }

    #[test]
    fn forced_copy_move_publishes_complete_file_and_removes_source() {
        let directory = test_directory("forced-copy");
        let source = directory.join("source.zip");
        let destination = directory.join("destination.zip");
        fs::write(&source, b"complete zip bytes").expect("write source");

        move_zip_no_overwrite_with_strategy(&source, &destination, true)
            .expect("copy fallback move");

        assert!(!source.exists());
        assert_eq!(
            b"complete zip bytes",
            fs::read(&destination).expect("read destination").as_slice()
        );
        assert!(
            fs::read_dir(&directory)
                .expect("read directory")
                .all(|entry| !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".partial"))
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn existing_destination_is_never_overwritten() {
        let directory = test_directory("no-overwrite");
        let source = directory.join("source.zip");
        let destination = directory.join("destination.zip");
        fs::write(&source, b"source").expect("write source");
        fs::write(&destination, b"existing").expect("write destination");

        assert!(move_zip_no_overwrite(&source, &destination).is_err());
        assert_eq!(
            b"source",
            fs::read(&source).expect("source remains").as_slice()
        );
        assert_eq!(
            b"existing",
            fs::read(&destination)
                .expect("destination remains")
                .as_slice()
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn archive_folder_replaces_windows_unsafe_characters_and_reserved_names() {
        assert_eq!("C106________", safe_archive_folder(Some(" C106:<>/\\|?* ")));
        assert_eq!("_CON", safe_archive_folder(Some("CON")));
        assert_eq!("_lpt9", safe_archive_folder(Some("lpt9")));
    }

    #[test]
    fn missing_or_empty_event_uses_uncategorized_folder() {
        assert_eq!("未分類", safe_archive_folder(None));
        assert_eq!("未分類", safe_archive_folder(Some(" . ")));
    }
}
