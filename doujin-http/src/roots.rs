//! Library root management endpoints.

use std::path::PathBuf;
use std::sync::TryLockError;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use doujin_files::RecycleBin;
use doujin_scanner::SourceKind;
use doujin_storage::roots::LibraryRootSnapshot;
use serde::{Deserialize, Serialize};

use crate::HttpState;
use crate::error::ApiError;
use crate::params::source_name;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegisterLibraryRootRequest {
    path: String,
    source: String,
    label: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateLibraryRootRequest {
    path: String,
    source: String,
    label: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct LibraryRootsResponse {
    roots: Vec<LibraryRootResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LibraryRootResponse {
    id: i64,
    path: String,
    source: &'static str,
    label: String,
    active: bool,
    created_at: String,
    updated_at: String,
}

impl From<LibraryRootSnapshot> for LibraryRootResponse {
    fn from(root: LibraryRootSnapshot) -> Self {
        Self {
            id: root.id,
            path: root.path.to_string_lossy().into_owned(),
            source: source_name(root.source),
            label: root.label,
            active: root.active,
            created_at: root.created_at,
            updated_at: root.updated_at,
        }
    }
}

pub(crate) async fn list_library_roots<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<LibraryRootsResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let roots = tokio::task::spawn_blocking(move || {
        let application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .library_roots()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(LibraryRootsResponse {
        roots: roots.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn register_library_root<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<RegisterLibraryRootRequest>, JsonRejection>,
) -> Result<Json<LibraryRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let source = parse_library_root_source(&payload.source)?;
    let path = PathBuf::from(payload.path);
    let label = payload.label.trim().to_owned();
    let root = tokio::task::spawn_blocking(move || {
        let mut application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .register_library_root(&path, source, &label)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

pub(crate) async fn update_library_root<R>(
    State(state): State<HttpState<R>>,
    Path(root_id): Path<String>,
    payload: Result<Json<UpdateLibraryRootRequest>, JsonRejection>,
) -> Result<Json<LibraryRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let root_id = parse_library_root_id(root_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let source = parse_library_root_source(&payload.source)?;
    let path = PathBuf::from(payload.path);
    let label = payload.label.trim().to_owned();
    let root = tokio::task::spawn_blocking(move || {
        let mut application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .update_library_root(root_id, &path, source, &label)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

pub(crate) async fn reactivate_library_root<R>(
    State(state): State<HttpState<R>>,
    Path(root_id): Path<String>,
) -> Result<Json<LibraryRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let root_id = parse_library_root_id(root_id)?;
    let root = tokio::task::spawn_blocking(move || {
        let mut application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .reactivate_library_root(root_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

pub(crate) async fn deactivate_library_root<R>(
    State(state): State<HttpState<R>>,
    Path(root_id): Path<String>,
) -> Result<Json<LibraryRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let root_id = parse_library_root_id(root_id)?;
    let root = tokio::task::spawn_blocking(move || {
        let mut application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .deactivate_library_root(root_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

pub(crate) fn parse_library_root_id(root_id: String) -> Result<i64, ApiError> {
    root_id
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ApiError::bad_request("invalid_library_root_id", "library root ID 必須是正整數")
        })
}

pub(crate) fn parse_library_root_source(source: &str) -> Result<SourceKind, ApiError> {
    match source {
        "archive" => Ok(SourceKind::Archive),
        "downloads" => Ok(SourceKind::Downloads),
        _ => Err(ApiError::bad_request(
            "invalid_library_root_source",
            "library root source 必須是 archive 或 downloads",
        )),
    }
}
