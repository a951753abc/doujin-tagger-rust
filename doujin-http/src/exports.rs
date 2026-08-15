//! Export root and export job endpoints.

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use doujin_app::ApplicationService;
use doujin_app::export::{
    ExportExecutionRequest, ExportPreflight, ExportProgress, write_export_package,
};
use doujin_files::RecycleBin;
use doujin_storage::{ExportJobItemSnapshot, ExportJobSnapshot, ExportRootSnapshot};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::params::{positive_id, validated_collection_ids};
use crate::{HttpState, SharedApplication, lock_interactive_application};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportRootRequest {
    path: String,
    label: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExportRootsResponse {
    roots: Vec<ExportRootResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExportRootResponse {
    id: i64,
    path: String,
    label: String,
    active: bool,
    created_at: String,
    updated_at: String,
}

impl From<ExportRootSnapshot> for ExportRootResponse {
    fn from(root: ExportRootSnapshot) -> Self {
        Self {
            id: root.id,
            path: root.path.to_string_lossy().into_owned(),
            label: root.label,
            active: root.active,
            created_at: root.created_at,
            updated_at: root.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportPackageRequest {
    collection_ids: Vec<i64>,
    export_root_id: i64,
    package_filename: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExportJobEnvelope {
    job: Option<ExportJobResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExportJobResponse {
    id: i64,
    export_root_id: i64,
    package_filename: String,
    status: &'static str,
    total_items: usize,
    processed_items: usize,
    total_bytes: u64,
    processed_bytes: u64,
    current_collection_id: Option<i64>,
    succeeded_items: usize,
    failed_items: usize,
    attempts: usize,
    error_message: Option<String>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
    items: Vec<ExportJobItemResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExportJobItemResponse {
    collection_id: i64,
    package_entry: String,
    original_filename: String,
    status: &'static str,
    source_size: u64,
    bytes_copied: u64,
    error_message: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenExportLocationResponse {
    job_id: i64,
    launched: bool,
}

pub(crate) async fn list_export_roots<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<ExportRootsResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let roots = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .export_roots()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(ExportRootsResponse {
        roots: roots.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn register_export_root<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<ExportRootRequest>, JsonRejection>,
) -> Result<Json<ExportRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let root = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .register_export_root(&PathBuf::from(payload.path), &payload.label)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

pub(crate) async fn update_export_root<R>(
    State(state): State<HttpState<R>>,
    Path(root_id): Path<String>,
    payload: Result<Json<ExportRootRequest>, JsonRejection>,
) -> Result<Json<ExportRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let root_id = parse_export_id(root_id, "invalid_export_root_id", "export root ID")?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let root = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .update_export_root(root_id, &PathBuf::from(payload.path), &payload.label)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

pub(crate) async fn deactivate_export_root<R>(
    State(state): State<HttpState<R>>,
    Path(root_id): Path<String>,
) -> Result<Json<ExportRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    set_export_root_active(state, root_id, false).await
}

pub(crate) async fn reactivate_export_root<R>(
    State(state): State<HttpState<R>>,
    Path(root_id): Path<String>,
) -> Result<Json<ExportRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    set_export_root_active(state, root_id, true).await
}

pub(crate) async fn set_export_root_active<R>(
    state: HttpState<R>,
    root_id: String,
    active: bool,
) -> Result<Json<ExportRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let root_id = parse_export_id(root_id, "invalid_export_root_id", "export root ID")?;
    let root = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        if active {
            application.reactivate_export_root(root_id)
        } else {
            application.deactivate_export_root(root_id)
        }
        .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

pub(crate) async fn preflight_export<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<ExportPackageRequest>, JsonRejection>,
) -> Result<Json<ExportPreflight>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let request = validated_export_request(payload)?;
    let preflight = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .export_preflight(
                &request.collection_ids,
                request.export_root_id,
                &request.package_filename,
            )
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(preflight))
}

pub(crate) async fn create_export<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<ExportPackageRequest>, JsonRejection>,
) -> Result<Json<ExportJobResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let request = validated_export_request(payload)?;
    let application = Arc::clone(&state.application);
    let (execution, response) = tokio::task::spawn_blocking(move || {
        let mut application_guard = lock_interactive_application(&application)?;
        let job = application_guard
            .enqueue_export(
                &request.collection_ids,
                request.export_root_id,
                &request.package_filename,
            )
            .map_err(ApiError::from_application)?;
        let execution = match application_guard.prepare_export_execution(job.id) {
            Ok(execution) => execution,
            Err(error) => {
                let message = error.to_string();
                let _ = application_guard.fail_export(job.id, None, &message);
                return Err(ApiError::from_application(error));
            }
        };
        let response = export_job_response(&application_guard, job.id)?;
        Ok((execution, response))
    })
    .await
    .map_err(|_| ApiError::internal())??;
    spawn_export_worker(Arc::clone(&state.application), execution);
    Ok(Json(response))
}

pub(crate) async fn get_current_export<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<ExportJobEnvelope>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .latest_export_job()
            .map_err(ApiError::from_application)?
            .map(|job| export_job_response(&application, job.id))
            .transpose()
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(ExportJobEnvelope { job }))
}

pub(crate) async fn get_export<R>(
    State(state): State<HttpState<R>>,
    Path(job_id): Path<String>,
) -> Result<Json<ExportJobResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job_id = parse_export_id(job_id, "invalid_export_job_id", "export job ID")?;
    let job = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        export_job_response(&application, job_id)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(job))
}

pub(crate) async fn retry_export<R>(
    State(state): State<HttpState<R>>,
    Path(job_id): Path<String>,
) -> Result<Json<ExportJobResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job_id = parse_export_id(job_id, "invalid_export_job_id", "export job ID")?;
    let application = Arc::clone(&state.application);
    let (execution, response) = tokio::task::spawn_blocking(move || {
        let mut application_guard = lock_interactive_application(&application)?;
        application_guard
            .retry_export(job_id)
            .map_err(ApiError::from_application)?;
        let execution = match application_guard.prepare_export_execution(job_id) {
            Ok(execution) => execution,
            Err(error) => {
                let message = error.to_string();
                let _ = application_guard.fail_export(job_id, None, &message);
                return Err(ApiError::from_application(error));
            }
        };
        let response = export_job_response(&application_guard, job_id)?;
        Ok((execution, response))
    })
    .await
    .map_err(|_| ApiError::internal())??;
    spawn_export_worker(Arc::clone(&state.application), execution);
    Ok(Json(response))
}

pub(crate) async fn open_export_location<R>(
    State(state): State<HttpState<R>>,
    Path(job_id): Path<String>,
) -> Result<Json<OpenExportLocationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job_id = parse_export_id(job_id, "invalid_export_job_id", "export job ID")?;
    tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .open_export_location(job_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(OpenExportLocationResponse {
        job_id,
        launched: true,
    }))
}

pub(crate) fn validated_export_request(
    payload: Result<Json<ExportPackageRequest>, JsonRejection>,
) -> Result<ExportPackageRequest, ApiError> {
    let Json(mut request) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    request.collection_ids = validated_collection_ids(request.collection_ids)?;
    positive_id(
        request.export_root_id,
        "invalid_export_root_id",
        "export root ID 必須是正整數",
    )?;
    if request.package_filename.trim().is_empty() {
        return Err(ApiError::bad_request(
            "invalid_export_package_filename",
            "package_filename 不得為空白",
        ));
    }
    Ok(request)
}

pub(crate) fn parse_export_id(
    value: String,
    code: &'static str,
    label: &str,
) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request(code, &format!("{label} 必須是正整數")))
}

pub(crate) fn export_job_response<R: RecycleBin>(
    application: &ApplicationService<R>,
    job_id: i64,
) -> Result<ExportJobResponse, ApiError> {
    let job = application
        .export_job(job_id)
        .map_err(ApiError::from_application)?;
    let items = application
        .repository()
        .export_job_items(job_id)
        .map_err(ApiError::from_storage)?;
    Ok(ExportJobResponse::from_snapshots(job, items))
}

impl ExportJobResponse {
    fn from_snapshots(job: ExportJobSnapshot, items: Vec<ExportJobItemSnapshot>) -> Self {
        Self {
            id: job.id,
            export_root_id: job.export_root_id,
            package_filename: job.package_filename,
            status: job.status.as_str(),
            total_items: job.total_items,
            processed_items: job.processed_items,
            total_bytes: job.total_bytes,
            processed_bytes: job.processed_bytes,
            current_collection_id: job.current_collection_id,
            succeeded_items: job.succeeded_items,
            failed_items: job.failed_items,
            attempts: job.attempts,
            error_message: job.error_message,
            created_at: job.created_at,
            updated_at: job.updated_at,
            completed_at: job.completed_at,
            items: items
                .into_iter()
                .map(|item| ExportJobItemResponse {
                    collection_id: item.collection_id,
                    package_entry: item.package_entry,
                    original_filename: item.original_filename,
                    status: item.status.as_str(),
                    source_size: item.source_size,
                    bytes_copied: item.bytes_copied,
                    error_message: item.error_message,
                })
                .collect(),
        }
    }
}

pub(crate) fn spawn_export_worker<R>(
    application: SharedApplication<R>,
    request: ExportExecutionRequest,
) where
    R: RecycleBin + Send + 'static,
{
    thread::spawn(move || {
        let job_id = request.job_id;
        let result = write_export_package(&request, |collection_id, event| {
            let mut application = application
                .lock()
                .map_err(|_| "application service lock 已損壞".to_owned())?;
            match event {
                ExportProgress::Started => application
                    .start_export_item(job_id, collection_id)
                    .map_err(|error| error.to_string()),
                ExportProgress::Bytes(bytes) => application
                    .update_export_progress(job_id, collection_id, bytes)
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
                ExportProgress::Completed => application
                    .complete_export_item(job_id, collection_id)
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
            }
        });
        let Ok(mut application) = application.lock() else {
            return;
        };
        match result {
            Ok(output) => {
                if let Err(error) = persist_completed_output(&output, || {
                    application.complete_export(job_id).map(|_| ())
                }) {
                    let _ = application.fail_export(job_id, None, &error.to_string());
                }
            }
            Err(error) => {
                let _ = application.fail_export(job_id, error.collection_id, &error.message);
            }
        }
    });
}

pub(crate) fn persist_completed_output<E, F>(
    output: &std::path::Path,
    complete: F,
) -> Result<(), String>
where
    E: fmt::Display,
    F: FnOnce() -> Result<(), E>,
{
    if let Err(error) = complete() {
        return match fs::remove_file(output) {
            Ok(()) => Err(error.to_string()),
            Err(cleanup_error) => Err(format!(
                "{error}；移除未完成登錄的 package 失敗：{cleanup_error}"
            )),
        };
    }
    Ok(())
}

#[cfg(test)]
mod export_worker_tests {
    use super::persist_completed_output;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn database_completion_failure_removes_formal_package() {
        let output = std::env::temp_dir().join(format!(
            "doujin-export-db-failure-{}-{}.zip",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::write(&output, b"complete zip bytes").expect("write output");

        let error = persist_completed_output(&output, || Err("injected database failure"))
            .expect_err("database completion must fail");

        assert!(error.contains("injected database failure"));
        assert!(!output.exists(), "formal output must be rolled back");
    }
}
