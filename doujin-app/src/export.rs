//! Safe ZIP-of-ZIPs export planning and streaming package creation.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use doujin_files::RecycleBin;
use doujin_storage::{ExportJobSnapshot, ExportRootSnapshot, NewExportJobItem, StorageError};
use doujin_thumbnails::duplicate_source_fingerprint;
use serde::{Deserialize, Serialize};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use crate::{ApplicationError, ApplicationResult, ApplicationService};

pub const EXPORT_FORMAT_VERSION: u32 = 1;
const COPY_BUFFER_SIZE: usize = 64 * 1024;
const WINDOWS_SAFE_PATH_UNITS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPreflightRequest {
    pub collection_ids: Vec<i64>,
    pub export_root_id: i64,
    pub package_filename: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportPreflightStatus {
    Exportable,
    Missing,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportPreflightItem {
    pub collection_id: i64,
    pub original_filename: String,
    pub package_entry: Option<String>,
    pub status: ExportPreflightStatus,
    pub source_size: u64,
    pub sha256: Option<String>,
    pub reason: Option<String>,
    #[serde(skip)]
    source_path: Option<PathBuf>,
    #[serde(skip)]
    source_identity: Option<String>,
    #[serde(skip)]
    manifest_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportPreflight {
    pub export_root_id: i64,
    pub export_root_label: String,
    pub package_filename: String,
    pub selected: usize,
    pub exportable: usize,
    pub missing: usize,
    pub unsupported: usize,
    pub total_bytes: u64,
    pub estimated_bytes: u64,
    pub free_bytes: Option<u64>,
    pub package_collision: bool,
    pub can_start: bool,
    pub cancellation_supported: bool,
    pub items: Vec<ExportPreflightItem>,
}

#[derive(Debug, Clone)]
pub struct ExportExecutionItem {
    pub collection_id: i64,
    pub source_path: PathBuf,
    pub source_identity: String,
    pub source_size: u64,
    pub package_entry: String,
}

#[derive(Debug, Clone)]
pub struct ExportExecutionRequest {
    pub job_id: i64,
    pub export_root: PathBuf,
    pub package_filename: String,
    pub created_at: String,
    pub items: Vec<ExportExecutionItem>,
    pub manifest: Vec<u8>,
}

#[derive(Debug)]
pub struct ExportWriteError {
    pub collection_id: Option<i64>,
    pub message: String,
}

impl std::fmt::Display for ExportWriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ExportWriteError {}

#[derive(Debug, Serialize, Deserialize)]
struct ExportManifest {
    format_version: u32,
    created_at: String,
    items: Vec<serde_json::Value>,
}

impl<R: RecycleBin> ApplicationService<R> {
    pub fn export_roots(&self) -> ApplicationResult<Vec<ExportRootSnapshot>> {
        Ok(self.repository.export_roots()?)
    }

    pub fn register_export_root(
        &mut self,
        path: &Path,
        label: &str,
    ) -> ApplicationResult<ExportRootSnapshot> {
        Ok(self.repository.register_export_root(path, label)?)
    }

    pub fn update_export_root(
        &mut self,
        root_id: i64,
        path: &Path,
        label: &str,
    ) -> ApplicationResult<ExportRootSnapshot> {
        Ok(self.repository.update_export_root(root_id, path, label)?)
    }

    pub fn deactivate_export_root(
        &mut self,
        root_id: i64,
    ) -> ApplicationResult<ExportRootSnapshot> {
        Ok(self.repository.deactivate_export_root(root_id)?)
    }

    pub fn reactivate_export_root(
        &mut self,
        root_id: i64,
    ) -> ApplicationResult<ExportRootSnapshot> {
        Ok(self.repository.reactivate_export_root(root_id)?)
    }

    pub fn export_preflight(
        &self,
        collection_ids: &[i64],
        export_root_id: i64,
        requested_package_filename: &str,
    ) -> ApplicationResult<ExportPreflight> {
        let collection_ids = normalized_collection_ids(collection_ids)?;
        let root = self.repository.export_root(export_root_id)?;
        validate_live_export_root(&root)?;
        let package_filename = safe_zip_filename(requested_package_filename, "export.zip")?;
        let output = safe_destination(&root.path, &package_filename)?;
        let package_collision = output.exists();
        let mut items = Vec::with_capacity(collection_ids.len());
        for collection_id in collection_ids {
            let collection = match self.repository.collection(collection_id) {
                Ok(collection) => collection,
                Err(_) => {
                    items.push(unavailable_item(
                        collection_id,
                        ExportPreflightStatus::Missing,
                        "catalog 找不到 active collection",
                    ));
                    continue;
                }
            };
            let media_kind = self.repository.collection_media_kind(collection_id)?;
            if media_kind != "zip" {
                items.push(ExportPreflightItem {
                    collection_id,
                    original_filename: collection.filename,
                    package_entry: None,
                    status: ExportPreflightStatus::Unsupported,
                    source_size: 0,
                    sha256: None,
                    reason: Some("第一版匯出只支援實體 ZIP 收藏".to_owned()),
                    source_path: None,
                    source_identity: None,
                    manifest_json: None,
                });
                continue;
            }
            let metadata = match fs::symlink_metadata(&collection.path) {
                Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
                    metadata
                }
                Ok(_) => {
                    items.push(unavailable_item_with_name(
                        collection_id,
                        collection.filename,
                        ExportPreflightStatus::Unsupported,
                        "來源必須是一般 ZIP 檔案且不能是 symlink",
                    ));
                    continue;
                }
                Err(_) => {
                    items.push(unavailable_item_with_name(
                        collection_id,
                        collection.filename,
                        ExportPreflightStatus::Missing,
                        "來源 ZIP 已遺失或無法存取",
                    ));
                    continue;
                }
            };
            if !collection
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
            {
                items.push(unavailable_item_with_name(
                    collection_id,
                    collection.filename,
                    ExportPreflightStatus::Unsupported,
                    "來源副檔名不是 ZIP",
                ));
                continue;
            }
            let identity = match duplicate_source_fingerprint(&collection.path) {
                Ok(identity) => identity,
                Err(error) => {
                    items.push(unavailable_item_with_name(
                        collection_id,
                        collection.filename,
                        ExportPreflightStatus::Unsupported,
                        &error.to_string(),
                    ));
                    continue;
                }
            };
            let sha256 = self
                .repository
                .duplicate_fingerprint(collection_id)?
                .filter(|fingerprint| fingerprint.source_fingerprint == identity)
                .and_then(|fingerprint| fingerprint.file_sha256);
            let original_filename = collection.filename.clone();
            let manifest = serde_json::json!({
                "collection_id": collection.id,
                "package_entry": null,
                "original_filename": original_filename,
                "effective_metadata": {
                    "title": collection.title,
                    "circle": collection.circle,
                    "authors": collection.authors,
                    "event": collection.event,
                    "parody": collection.parody,
                },
                "tags": collection.tags,
                "source_size": metadata.len(),
                "sha256": sha256,
            });
            items.push(ExportPreflightItem {
                collection_id,
                original_filename,
                package_entry: None,
                status: ExportPreflightStatus::Exportable,
                source_size: metadata.len(),
                sha256,
                reason: None,
                source_path: Some(collection.path),
                source_identity: Some(identity),
                manifest_json: Some(manifest.to_string()),
            });
        }
        assign_package_entries(&mut items)?;
        let selected = items.len();
        let exportable = items
            .iter()
            .filter(|item| item.status == ExportPreflightStatus::Exportable)
            .count();
        let missing = items
            .iter()
            .filter(|item| item.status == ExportPreflightStatus::Missing)
            .count();
        let unsupported = selected - exportable - missing;
        let total_bytes = items.iter().map(|item| item.source_size).sum::<u64>();
        let estimated_bytes = total_bytes.saturating_add(4096);
        let free_bytes = fs2::available_space(&root.path).ok();
        Ok(ExportPreflight {
            export_root_id,
            export_root_label: root.label,
            package_filename,
            selected,
            exportable,
            missing,
            unsupported,
            total_bytes,
            estimated_bytes,
            free_bytes,
            package_collision,
            can_start: exportable == selected
                && !package_collision
                && free_bytes.is_none_or(|free| free >= estimated_bytes),
            cancellation_supported: false,
            items,
        })
    }

    pub fn enqueue_export(
        &mut self,
        collection_ids: &[i64],
        export_root_id: i64,
        package_filename: &str,
    ) -> ApplicationResult<ExportJobSnapshot> {
        let preflight = self.export_preflight(collection_ids, export_root_id, package_filename)?;
        if !preflight.can_start {
            return Err(StorageError::InvalidExportJob(
                "export preflight 未通過；請先處理來源、空間或名稱問題".to_owned(),
            )
            .into());
        }
        let items = preflight
            .items
            .iter()
            .map(|item| {
                let mut manifest: serde_json::Value = serde_json::from_str(
                    item.manifest_json
                        .as_deref()
                        .expect("exportable item has manifest"),
                )?;
                manifest["package_entry"] = serde_json::Value::String(
                    item.package_entry
                        .clone()
                        .expect("exportable item has package entry"),
                );
                Ok(NewExportJobItem {
                    collection_id: item.collection_id,
                    package_entry: item.package_entry.clone().expect("entry"),
                    original_filename: item.original_filename.clone(),
                    expected_source_identity: item.source_identity.clone().expect("identity"),
                    source_size: item.source_size,
                    manifest_json: manifest.to_string(),
                })
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        Ok(self.repository.create_export_job(
            export_root_id,
            &preflight.package_filename,
            &items,
        )?)
    }

    pub fn export_job(&self, job_id: i64) -> ApplicationResult<ExportJobSnapshot> {
        Ok(self.repository.export_job(job_id)?)
    }

    pub fn latest_export_job(&self) -> ApplicationResult<Option<ExportJobSnapshot>> {
        Ok(self.repository.latest_export_job()?)
    }

    pub fn prepare_export_execution(
        &mut self,
        job_id: i64,
    ) -> ApplicationResult<ExportExecutionRequest> {
        let job = self.repository.export_job(job_id)?;
        let root = self.repository.export_root(job.export_root_id)?;
        validate_live_export_root(&root)?;
        safe_destination(&root.path, &job.package_filename)?;
        let stored_items = self.repository.export_job_items(job.id)?;
        let mut manifest_items = Vec::with_capacity(stored_items.len());
        let mut items = Vec::with_capacity(stored_items.len());
        for item in stored_items {
            let source_path = self
                .repository
                .active_collection_file_path(item.collection_id)?;
            manifest_items.push(serde_json::from_str(&item.manifest_json)?);
            items.push(ExportExecutionItem {
                collection_id: item.collection_id,
                source_path,
                source_identity: item.expected_source_identity,
                source_size: item.source_size,
                package_entry: item.package_entry,
            });
        }
        let manifest = serde_json::to_vec_pretty(&ExportManifest {
            format_version: EXPORT_FORMAT_VERSION,
            created_at: job.created_at.clone(),
            items: manifest_items,
        })?;
        self.repository.start_export_job(job_id)?;
        Ok(ExportExecutionRequest {
            job_id,
            export_root: root.path,
            package_filename: job.package_filename,
            created_at: job.created_at,
            items,
            manifest,
        })
    }

    pub fn start_export_item(&mut self, job_id: i64, collection_id: i64) -> ApplicationResult<()> {
        self.repository
            .start_export_job_item(job_id, collection_id)?;
        Ok(())
    }

    pub fn update_export_progress(
        &mut self,
        job_id: i64,
        collection_id: i64,
        bytes: u64,
    ) -> ApplicationResult<ExportJobSnapshot> {
        Ok(self
            .repository
            .update_export_item_progress(job_id, collection_id, bytes)?)
    }

    pub fn complete_export_item(
        &mut self,
        job_id: i64,
        collection_id: i64,
    ) -> ApplicationResult<ExportJobSnapshot> {
        Ok(self
            .repository
            .complete_export_job_item(job_id, collection_id)?)
    }

    pub fn complete_export(&mut self, job_id: i64) -> ApplicationResult<ExportJobSnapshot> {
        Ok(self.repository.complete_export_job(job_id)?)
    }

    pub fn fail_export(
        &mut self,
        job_id: i64,
        collection_id: Option<i64>,
        message: &str,
    ) -> ApplicationResult<ExportJobSnapshot> {
        Ok(self
            .repository
            .fail_export_job(job_id, collection_id, message)?)
    }

    pub fn retry_export(&mut self, job_id: i64) -> ApplicationResult<ExportJobSnapshot> {
        Ok(self.repository.retry_export_job(job_id)?)
    }

    pub fn recover_interrupted_exports(&mut self) -> ApplicationResult<usize> {
        if let Some(job) = self.repository.latest_export_job()?
            && job.status == doujin_storage::ExportJobStatus::Running
            && let Ok(root) = self.repository.export_root(job.export_root_id)
            && let Ok(destination) = safe_destination(&root.path, &job.package_filename)
        {
            let partial = root.path.join(format!("{}.partial", job.package_filename));
            if partial.starts_with(&root.path) {
                match fs::remove_file(&partial) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(ApplicationError::ExportIo(error)),
                }
            }
            drop(destination);
        }
        Ok(self.repository.recover_interrupted_export_jobs()?)
    }

    pub fn open_export_location(&self, job_id: i64) -> ApplicationResult<()> {
        let job = self.repository.export_job(job_id)?;
        if job.status != doujin_storage::ExportJobStatus::Succeeded {
            return Err(StorageError::InvalidExportJob(
                "export job 尚未成功完成，不能開啟輸出位置".to_owned(),
            )
            .into());
        }
        let root = self.repository.export_root(job.export_root_id)?;
        validate_live_export_root(&root)?;
        self.launcher
            .open_default(&root.path)
            .map_err(ApplicationError::ExportIo)
    }
}

pub fn write_export_package<F>(
    request: &ExportExecutionRequest,
    mut progress: F,
) -> Result<PathBuf, ExportWriteError>
where
    F: FnMut(i64, ExportProgress) -> Result<(), String>,
{
    let destination = safe_destination(&request.export_root, &request.package_filename)
        .map_err(storage_export_error)?;
    if destination.exists() {
        return Err(export_error(None, "匯出 package 已存在，禁止覆寫"));
    }
    let partial = request
        .export_root
        .join(format!("{}.partial", request.package_filename));
    if partial.exists() {
        fs::remove_file(&partial)
            .map_err(|error| export_io_error(None, "清理舊 partial", &partial, error))?;
    }
    let result = write_partial(request, &partial, &mut progress).and_then(|()| {
        publish_partial_with(
            &partial,
            &destination,
            |from, to| fs::hard_link(from, to),
            |path| fs::remove_file(path),
        )?;
        Ok(destination.clone())
    });
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

fn publish_partial_with<L, D>(
    partial: &Path,
    destination: &Path,
    link: L,
    mut delete: D,
) -> Result<(), ExportWriteError>
where
    L: FnOnce(&Path, &Path) -> io::Result<()>,
    D: FnMut(&Path) -> io::Result<()>,
{
    link(partial, destination)
        .map_err(|error| export_io_error(None, "finalize package", destination, error))?;
    if let Err(error) = delete(partial) {
        let destination_cleanup = delete(destination);
        let partial_cleanup = delete(partial);
        let cleanup_detail = match (destination_cleanup, partial_cleanup) {
            (Ok(()), Ok(())) => String::new(),
            (destination, partial) => {
                format!("；rollback 結果：destination={destination:?}, partial={partial:?}")
            }
        };
        return Err(export_error(
            None,
            &format!("清理 partial 失敗：{error}{cleanup_detail}"),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportProgress {
    Started,
    Bytes(u64),
    Completed,
}

fn write_partial<F>(
    request: &ExportExecutionRequest,
    partial: &Path,
    progress: &mut F,
) -> Result<(), ExportWriteError>
where
    F: FnMut(i64, ExportProgress) -> Result<(), String>,
{
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial)
        .map_err(|error| export_io_error(None, "建立 partial", partial, error))?;
    let mut archive = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for item in &request.items {
        progress(item.collection_id, ExportProgress::Started)
            .map_err(|message| export_error(Some(item.collection_id), &message))?;
        validate_unchanged_source(item)?;
        archive
            .start_file(&item.package_entry, options)
            .map_err(|error| export_error(Some(item.collection_id), &error.to_string()))?;
        let mut source = File::open(&item.source_path).map_err(|error| {
            export_io_error(
                Some(item.collection_id),
                "開啟來源",
                &item.source_path,
                error,
            )
        })?;
        let mut buffer = [0_u8; COPY_BUFFER_SIZE];
        let mut copied = 0_u64;
        loop {
            let read = source.read(&mut buffer).map_err(|error| {
                export_io_error(
                    Some(item.collection_id),
                    "讀取來源",
                    &item.source_path,
                    error,
                )
            })?;
            if read == 0 {
                break;
            }
            archive.write_all(&buffer[..read]).map_err(|error| {
                export_io_error(Some(item.collection_id), "寫入 package", partial, error)
            })?;
            copied += read as u64;
            progress(item.collection_id, ExportProgress::Bytes(copied))
                .map_err(|message| export_error(Some(item.collection_id), &message))?;
        }
        if copied != item.source_size {
            return Err(export_error(
                Some(item.collection_id),
                "來源在匯出期間變更，已中止整個 package",
            ));
        }
        progress(item.collection_id, ExportProgress::Completed)
            .map_err(|message| export_error(Some(item.collection_id), &message))?;
    }
    archive
        .start_file("manifest.json", options)
        .map_err(|error| export_error(None, &error.to_string()))?;
    archive
        .write_all(&request.manifest)
        .map_err(|error| export_io_error(None, "寫入 manifest", partial, error))?;
    let file = archive
        .finish()
        .map_err(|error| export_error(None, &error.to_string()))?;
    file.sync_all()
        .map_err(|error| export_io_error(None, "同步 package", partial, error))
}

fn validate_unchanged_source(item: &ExportExecutionItem) -> Result<(), ExportWriteError> {
    let metadata = fs::symlink_metadata(&item.source_path).map_err(|error| {
        export_io_error(
            Some(item.collection_id),
            "讀取來源狀態",
            &item.source_path,
            error,
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != item.source_size
    {
        return Err(export_error(
            Some(item.collection_id),
            "來源遺失、類型或大小已變更，已中止整個 package",
        ));
    }
    let identity = duplicate_source_fingerprint(&item.source_path)
        .map_err(|error| export_error(Some(item.collection_id), &error.to_string()))?;
    if identity != item.source_identity {
        return Err(export_error(
            Some(item.collection_id),
            "來源自 preflight 後已變更，請重新預覽",
        ));
    }
    Ok(())
}

fn normalized_collection_ids(collection_ids: &[i64]) -> ApplicationResult<Vec<i64>> {
    let mut seen = HashSet::new();
    let ids = collection_ids
        .iter()
        .copied()
        .filter(|id| *id > 0 && seen.insert(*id))
        .collect::<Vec<_>>();
    if ids.is_empty() || ids.len() != collection_ids.len() {
        return Err(StorageError::InvalidExportJob(
            "collection IDs 必須是非空、無重複的正整數集合".to_owned(),
        )
        .into());
    }
    Ok(ids)
}

fn validate_live_export_root(root: &ExportRootSnapshot) -> ApplicationResult<()> {
    if !root.active {
        return Err(StorageError::InvalidExportRoot("匯出目的地已停用".to_owned()).into());
    }
    let metadata = fs::symlink_metadata(&root.path)
        .map_err(|error| StorageError::InvalidExportRoot(format!("匯出目的地無法存取：{error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StorageError::InvalidExportRoot(
            "匯出目的地必須是一般資料夾且不能是 symlink".to_owned(),
        )
        .into());
    }
    Ok(())
}

fn safe_destination(root: &Path, filename: &str) -> Result<PathBuf, StorageError> {
    if Path::new(filename).components().count() != 1
        || !matches!(
            Path::new(filename).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(StorageError::InvalidExportJob(
            "package filename 不得包含路徑".to_owned(),
        ));
    }
    let destination = root.join(filename);
    if !destination.starts_with(root) {
        return Err(StorageError::InvalidExportJob(
            "package 目的地超出 registered export root".to_owned(),
        ));
    }
    if destination.to_string_lossy().encode_utf16().count() > WINDOWS_SAFE_PATH_UNITS {
        return Err(StorageError::InvalidExportJob(
            "package 路徑過長，請縮短檔名或目的地".to_owned(),
        ));
    }
    Ok(destination)
}

pub fn safe_zip_filename(requested: &str, fallback: &str) -> Result<String, StorageError> {
    let mut stem = requested.trim().to_owned();
    if stem.to_ascii_lowercase().ends_with(".zip") {
        stem.truncate(stem.len() - 4);
    }
    stem = stem
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    stem = stem.trim_end_matches([' ', '.']).trim().to_owned();
    if stem.is_empty() {
        stem = fallback.trim_end_matches(".zip").to_owned();
    }
    let base = stem.split('.').next().unwrap_or_default();
    if is_windows_reserved(base) {
        stem.insert(0, '_');
    }
    if stem.encode_utf16().count() > 160 {
        return Err(StorageError::InvalidExportJob(
            "package filename 過長".to_owned(),
        ));
    }
    Ok(format!("{stem}.zip"))
}

pub fn validate_package_filename(requested: &str) -> Result<String, StorageError> {
    safe_zip_filename(requested, "export.zip")
}

fn is_windows_reserved(base: &str) -> bool {
    let upper = base.trim().to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit()
            && upper.as_bytes()[3] != b'0')
}

fn assign_package_entries(items: &mut [ExportPreflightItem]) -> ApplicationResult<()> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        if item.status == ExportPreflightStatus::Exportable {
            let safe = safe_zip_filename(
                &item.original_filename,
                &format!("collection-{}", item.collection_id),
            )?;
            groups.entry(safe.to_lowercase()).or_default().push(index);
        }
    }
    for indexes in groups.values() {
        for index in indexes {
            let item = &mut items[*index];
            let safe = safe_zip_filename(
                &item.original_filename,
                &format!("collection-{}", item.collection_id),
            )?;
            item.package_entry = Some(if indexes.len() == 1 {
                safe
            } else {
                let stem = safe.strip_suffix(".zip").unwrap_or(&safe);
                format!("{stem} [collection-{}].zip", item.collection_id)
            });
        }
    }
    Ok(())
}

fn unavailable_item(
    collection_id: i64,
    status: ExportPreflightStatus,
    reason: &str,
) -> ExportPreflightItem {
    unavailable_item_with_name(
        collection_id,
        format!("collection-{collection_id}"),
        status,
        reason,
    )
}

fn unavailable_item_with_name(
    collection_id: i64,
    original_filename: String,
    status: ExportPreflightStatus,
    reason: &str,
) -> ExportPreflightItem {
    ExportPreflightItem {
        collection_id,
        original_filename,
        package_entry: None,
        status,
        source_size: 0,
        sha256: None,
        reason: Some(reason.to_owned()),
        source_path: None,
        source_identity: None,
        manifest_json: None,
    }
}

fn storage_export_error(error: StorageError) -> ExportWriteError {
    export_error(None, &error.to_string())
}

fn export_error(collection_id: Option<i64>, message: &str) -> ExportWriteError {
    ExportWriteError {
        collection_id,
        message: message.to_owned(),
    }
}

fn export_io_error(
    collection_id: Option<i64>,
    action: &str,
    path: &Path,
    error: io::Error,
) -> ExportWriteError {
    export_error(
        collection_id,
        &format!("{action}失敗：{}：{error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TestTree(PathBuf);

    impl TestTree {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "doujin-export-app-{label}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("test tree");
            Self(path)
        }

        fn directory(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(&path).expect("directory");
            path
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            if self
                .0
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("doujin-export-app-"))
            {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn package_filename_sanitizes_windows_illegal_characters_and_path_separators() {
        assert_eq!(
            "C106_精選_最終.zip",
            validate_package_filename(" C106:精選/最終.zip ").expect("safe filename")
        );
        let traversal = validate_package_filename("../outside.zip").expect("sanitized traversal");
        assert_eq!(".._outside.zip", traversal);
        assert_eq!(1, Path::new(&traversal).components().count());
    }

    #[test]
    fn package_filename_handles_reserved_trailing_and_extension_predictably() {
        assert_eq!(
            "_CON.zip",
            validate_package_filename("CON. ").expect("reserved")
        );
        assert_eq!(
            "_LPT1.zip",
            validate_package_filename("LPT1.zip").expect("reserved")
        );
        assert_eq!(
            "books.zip",
            validate_package_filename("books").expect("extension")
        );
        assert_eq!(
            "books.zip",
            validate_package_filename("books.ZIP").expect("extension")
        );
        assert_eq!(
            "export.zip",
            validate_package_filename(" . ").expect("fallback")
        );
    }

    #[test]
    fn package_filename_rejects_excessive_length() {
        assert!(matches!(
            validate_package_filename(&"a".repeat(161)),
            Err(StorageError::InvalidExportJob(_))
        ));
    }

    #[test]
    fn internal_basename_collisions_receive_stable_collection_suffixes() {
        let mut items = vec![
            exportable_item(9, "same.zip"),
            exportable_item(2, "SAME.ZIP"),
            exportable_item(4, "unique.zip"),
        ];
        assign_package_entries(&mut items).expect("entries");
        assert_eq!(
            Some("same [collection-9].zip"),
            items[0].package_entry.as_deref()
        );
        assert_eq!(
            Some("SAME [collection-2].zip"),
            items[1].package_entry.as_deref()
        );
        assert_eq!(Some("unique.zip"), items[2].package_entry.as_deref());
    }

    #[test]
    fn writer_streams_one_hundred_one_sources_as_stored_entries_with_safe_manifest() {
        let tree = TestTree::new("writer-large");
        let sources = tree.directory("sources");
        let exports = tree.directory("exports");
        let mut items = Vec::new();
        let mut original_bytes = Vec::new();
        let mut original_modified = Vec::new();
        for collection_id in 1..=101 {
            let source = sources.join(format!("book-{collection_id:03}.zip"));
            let bytes = vec![collection_id as u8; 8192 + collection_id as usize];
            fs::write(&source, &bytes).expect("source");
            original_modified.push(
                fs::metadata(&source)
                    .expect("metadata")
                    .modified()
                    .expect("mtime"),
            );
            original_bytes.push(bytes);
            items.push(execution_item(collection_id, source));
        }
        let manifest = serde_json::to_vec_pretty(&serde_json::json!({
            "format_version": EXPORT_FORMAT_VERSION,
            "created_at": "2026-08-13T00:00:00Z",
            "items": [
                {"collection_id": 1, "package_entry": "book-001.zip", "sha256": null},
                {"collection_id": 2, "package_entry": "book-002.zip", "sha256": "a".repeat(64)}
            ]
        }))
        .expect("manifest");
        let request = ExportExecutionRequest {
            job_id: 8,
            export_root: exports.clone(),
            package_filename: "archive.zip".to_owned(),
            created_at: "2026-08-13T00:00:00Z".to_owned(),
            items,
            manifest,
        };
        let mut events = Vec::new();
        let output = write_export_package(&request, |collection_id, event| {
            events.push((collection_id, event));
            Ok(())
        })
        .expect("write package");
        assert_eq!(
            101,
            events
                .iter()
                .filter(|(_, event)| *event == ExportProgress::Started)
                .count()
        );
        assert_eq!(
            101,
            events
                .iter()
                .filter(|(_, event)| *event == ExportProgress::Completed)
                .count()
        );
        assert!(!exports.join("archive.zip.partial").exists());

        let mut archive =
            zip::ZipArchive::new(File::open(output).expect("output")).expect("outer ZIP");
        assert_eq!(102, archive.len());
        for index in 0..101 {
            let entry = archive.by_index(index).expect("source entry");
            assert_eq!(CompressionMethod::Stored, entry.compression());
            assert_eq!(entry.size(), entry.compressed_size());
        }
        let mut manifest_text = String::new();
        archive
            .by_name("manifest.json")
            .expect("manifest entry")
            .read_to_string(&mut manifest_text)
            .expect("manifest text");
        assert!(!manifest_text.contains(&tree.0.to_string_lossy().to_string()));
        assert!(manifest_text.contains(&"a".repeat(64)));
        assert!(manifest_text.contains("\"sha256\": null"));

        for (index, item) in request.items.iter().enumerate() {
            assert_eq!(
                original_bytes[index],
                fs::read(&item.source_path).expect("source unchanged")
            );
            assert_eq!(
                original_modified[index],
                fs::metadata(&item.source_path)
                    .expect("metadata")
                    .modified()
                    .expect("mtime")
            );
        }
    }

    #[test]
    fn source_changed_fails_whole_package_and_cleans_partial() {
        let tree = TestTree::new("source-changed");
        let source = tree.0.join("book.zip");
        fs::write(&source, b"before").expect("source");
        let mut item = execution_item(17, source.clone());
        item.source_identity = "stale identity".to_owned();
        let request = execution_request(&tree, "changed.zip", vec![item]);
        let error = write_export_package(&request, |_, _| Ok(())).expect_err("must fail");
        assert_eq!(Some(17), error.collection_id);
        assert!(!tree.0.join("changed.zip").exists());
        assert!(!tree.0.join("changed.zip.partial").exists());
        assert_eq!(
            b"before",
            fs::read(source).expect("source preserved").as_slice()
        );
    }

    #[test]
    fn missing_source_fails_whole_package_and_cleans_partial() {
        let tree = TestTree::new("source-missing");
        let source = tree.0.join("missing.zip");
        let request = ExportExecutionRequest {
            job_id: 3,
            export_root: tree.0.clone(),
            package_filename: "missing-package.zip".to_owned(),
            created_at: "now".to_owned(),
            items: vec![ExportExecutionItem {
                collection_id: 23,
                source_path: source,
                source_identity: "missing".to_owned(),
                source_size: 4,
                package_entry: "missing.zip".to_owned(),
            }],
            manifest: b"{}".to_vec(),
        };
        assert!(write_export_package(&request, |_, _| Ok(())).is_err());
        assert!(!tree.0.join("missing-package.zip").exists());
        assert!(!tree.0.join("missing-package.zip.partial").exists());
    }

    #[test]
    fn existing_destination_is_never_overwritten_or_renamed() {
        let tree = TestTree::new("no-overwrite");
        let source = tree.0.join("book.zip");
        fs::write(&source, b"source").expect("source");
        let destination = tree.0.join("existing.zip");
        fs::write(&destination, b"sentinel").expect("destination");
        let request = execution_request(&tree, "existing.zip", vec![execution_item(1, source)]);
        assert!(write_export_package(&request, |_, _| Ok(())).is_err());
        assert_eq!(
            b"sentinel",
            fs::read(destination)
                .expect("destination unchanged")
                .as_slice()
        );
        assert!(!tree.0.join("existing.zip.partial").exists());
    }

    #[test]
    fn progress_callback_failure_aborts_atomically_and_cleans_partial() {
        let tree = TestTree::new("progress-failure");
        let source = tree.0.join("book.zip");
        fs::write(&source, vec![7; COPY_BUFFER_SIZE * 2 + 1]).expect("source");
        let request = execution_request(&tree, "progress.zip", vec![execution_item(1, source)]);
        let error = write_export_package(&request, |_, event| match event {
            ExportProgress::Bytes(_) => Err("job state write failed".to_owned()),
            _ => Ok(()),
        })
        .expect_err("callback failure");
        assert!(error.message.contains("job state write failed"));
        assert!(!tree.0.join("progress.zip").exists());
        assert!(!tree.0.join("progress.zip.partial").exists());
    }

    #[test]
    fn finalize_cleanup_failure_rolls_back_formal_and_partial_files() {
        let tree = TestTree::new("finalize-cleanup-failure");
        let partial = tree.0.join("package.zip.partial");
        let destination = tree.0.join("package.zip");
        fs::write(&partial, b"complete zip bytes").expect("partial");
        let mut injected = false;

        let error = publish_partial_with(
            &partial,
            &destination,
            |from, to| fs::hard_link(from, to),
            |path| {
                if path == partial && !injected {
                    injected = true;
                    Err(io::Error::other("injected partial cleanup failure"))
                } else {
                    fs::remove_file(path)
                }
            },
        )
        .expect_err("cleanup failure must roll back publication");

        assert!(error.message.contains("injected partial cleanup failure"));
        assert!(!destination.exists(), "formal output must be rolled back");
        assert!(!partial.exists(), "partial output must be cleaned");
    }

    fn execution_item(collection_id: i64, source_path: PathBuf) -> ExportExecutionItem {
        let metadata = fs::metadata(&source_path).expect("metadata");
        ExportExecutionItem {
            collection_id,
            source_identity: duplicate_source_fingerprint(&source_path).expect("identity"),
            source_size: metadata.len(),
            package_entry: format!("book-{collection_id:03}.zip"),
            source_path,
        }
    }

    fn execution_request(
        tree: &TestTree,
        filename: &str,
        items: Vec<ExportExecutionItem>,
    ) -> ExportExecutionRequest {
        ExportExecutionRequest {
            job_id: 1,
            export_root: tree.0.clone(),
            package_filename: filename.to_owned(),
            created_at: "now".to_owned(),
            items,
            manifest: b"{\"format_version\":1,\"items\":[]}".to_vec(),
        }
    }

    fn exportable_item(collection_id: i64, filename: &str) -> ExportPreflightItem {
        ExportPreflightItem {
            collection_id,
            original_filename: filename.to_owned(),
            package_entry: None,
            status: ExportPreflightStatus::Exportable,
            source_size: 1,
            sha256: None,
            reason: None,
            source_path: Some(PathBuf::from(filename)),
            source_identity: Some("identity".to_owned()),
            manifest_json: Some("{}".to_owned()),
        }
    }
}
