//! File action (move/rename/delete) endpoints.

use std::sync::TryLockError;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use doujin_app::archive::ArchiveMovePreflight;
use doujin_app::rename::{RenameExpectedItem, RenamePreflight};
use doujin_files::{BatchReport, DeleteRequest, ItemStatus, RecycleBin};
use doujin_storage::lifecycle::DeleteMode;
use serde::{Deserialize, Serialize};

use crate::HttpState;
use crate::error::ApiError;
use crate::params::{positive_id, validated_collection_ids};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MoveCollectionsRequest {
    collection_ids: Vec<i64>,
    archive_root_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MovePreflightRequest {
    collection_ids: Vec<i64>,
    archive_root_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteCollectionsRequest {
    collection_ids: Vec<i64>,
    mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenamePreflightRequest {
    collection_ids: Vec<i64>,
    template: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameCollectionsRequest {
    template: String,
    items: Vec<RenameExpectedItem>,
}

#[derive(Debug, Serialize)]
pub(crate) struct FileActionBatchResponse {
    succeeded: usize,
    failed: usize,
    pending_recovery: usize,
    items: Vec<FileActionItemResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct FileActionItemResponse {
    collection_id: i64,
    operation_id: Option<i64>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl From<BatchReport> for FileActionBatchResponse {
    fn from(report: BatchReport) -> Self {
        Self {
            succeeded: report.succeeded(),
            failed: report.failed(),
            pending_recovery: report.pending_recovery(),
            items: report
                .items
                .into_iter()
                .map(|item| FileActionItemResponse {
                    collection_id: item.collection_id,
                    operation_id: item.operation_id,
                    status: match item.status {
                        ItemStatus::Succeeded => "succeeded",
                        ItemStatus::Failed => "failed",
                        ItemStatus::PendingRecovery => "pending_recovery",
                    },
                    error: item.error,
                })
                .collect(),
        }
    }
}

pub(crate) async fn move_collections<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<MoveCollectionsRequest>, JsonRejection>,
) -> Result<Json<FileActionBatchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validated_collection_ids(payload.collection_ids)?;
    let archive_root_id = positive_id(
        payload.archive_root_id,
        "invalid_archive_root_id",
        "archive root ID 必須是正整數",
    )?;
    let report = tokio::task::spawn_blocking(move || {
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
        Ok(application.move_collections_to_archive(&collection_ids, archive_root_id))
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(report.into()))
}

pub(crate) async fn preflight_move_collections<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<MovePreflightRequest>, JsonRejection>,
) -> Result<Json<ArchiveMovePreflight>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validated_collection_ids(payload.collection_ids)?;
    let archive_root_id = positive_id(
        payload.archive_root_id,
        "invalid_archive_root_id",
        "archive root ID 必須是正整數",
    )?;
    let preflight = tokio::task::spawn_blocking(move || {
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
            .move_preflight(&collection_ids, archive_root_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(preflight))
}

pub(crate) async fn preflight_rename_collections<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<RenamePreflightRequest>, JsonRejection>,
) -> Result<Json<RenamePreflight>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validated_collection_ids(payload.collection_ids)?;
    let template = payload.template;
    if template.trim().is_empty() {
        return Err(ApiError::bad_request(
            "invalid_rename_template",
            "rename template 不得為空",
        ));
    }
    let preflight = tokio::task::spawn_blocking(move || {
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
            .rename_preflight(&collection_ids, &template)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(preflight))
}

pub(crate) async fn rename_collections<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<RenameCollectionsRequest>, JsonRejection>,
) -> Result<Json<FileActionBatchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    if payload.template.trim().is_empty() || payload.items.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_rename_request",
            "rename template 與 preflight items 不得為空",
        ));
    }
    validated_collection_ids(
        payload
            .items
            .iter()
            .map(|item| item.collection_id)
            .collect(),
    )?;
    let template = payload.template;
    let items = payload.items;
    let report = tokio::task::spawn_blocking(move || {
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
            .apply_rename_preflight(&template, &items)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(report.into()))
}

pub(crate) async fn delete_collections<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<DeleteCollectionsRequest>, JsonRejection>,
) -> Result<Json<FileActionBatchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validated_collection_ids(payload.collection_ids)?;
    let mode = match payload.mode.as_str() {
        "soft" => DeleteMode::Soft,
        "permanent" => DeleteMode::Permanent,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_delete_mode",
                "delete mode 必須是 soft 或 permanent",
            ));
        }
    };
    let requests: Vec<_> = collection_ids
        .into_iter()
        .map(|collection_id| DeleteRequest {
            collection_id,
            mode,
        })
        .collect();
    let report = tokio::task::spawn_blocking(move || {
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
        Ok(application.delete_collections(&requests))
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(report.into()))
}
