//! Library scan endpoints.

use std::sync::TryLockError;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use doujin_app::{
    ApplicationScanExpectation, ApplicationScanMode, ApplicationScanOptions,
    ApplicationScanPreflight, ApplicationScanReport,
};
use doujin_files::RecycleBin;
use doujin_storage::scan::{ScanIssueSnapshot, ScanRunSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ApiError;
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartScanRequest {
    mode: Option<String>,
    expected: Option<ApplicationScanExpectation>,
}

pub(crate) async fn preflight_scan<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<ApplicationScanPreflight>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let result = tokio::task::spawn_blocking(move || {
        let application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::conflict(
                    "scan_already_running",
                    "重新掃描正在執行",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        let roots = application
            .repository()
            .active_scan_roots()
            .map_err(ApiError::from_storage)?;
        application
            .preflight_scan(&roots)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(result))
}

pub(crate) async fn start_scan<R>(
    State(state): State<HttpState<R>>,
    payload: Bytes,
) -> Result<Json<ApplicationScanReport>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let request = if payload.is_empty() {
        StartScanRequest::default()
    } else {
        serde_json::from_slice(&payload)
            .map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?
    };
    let mode = match request.mode.as_deref().unwrap_or("apply_safe_renames") {
        "apply_safe_renames" => ApplicationScanMode::ApplySafeRenames,
        "no_rename" => ApplicationScanMode::NoRename,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_scan_mode",
                "scan mode 必須是 apply_safe_renames 或 no_rename",
            ));
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        let mut application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::conflict(
                    "scan_already_running",
                    "重新掃描正在執行",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        let roots = application
            .repository()
            .active_scan_roots()
            .map_err(ApiError::from_storage)?;
        application
            .run_scan_with_options(
                &roots,
                ApplicationScanOptions {
                    mode,
                    expected: request.expected,
                },
            )
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(result))
}

#[derive(Debug, Serialize)]
pub(crate) struct LatestScanEnvelope {
    scan: Option<ScanRunResponse>,
}

pub(crate) async fn get_latest_scan<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<LatestScanEnvelope>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let scan = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        let Some(run) = application
            .repository()
            .latest_scan_run()
            .map_err(ApiError::from_storage)?
        else {
            return Ok(None);
        };
        let issues = application
            .repository()
            .scan_issues(run.id)
            .map_err(ApiError::from_storage)?;
        ScanRunResponse::from_snapshots(run, issues).map(Some)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(LatestScanEnvelope { scan }))
}

pub(crate) async fn get_scan<R>(
    State(state): State<HttpState<R>>,
    Path(scan_run_id): Path<String>,
) -> Result<Json<ScanRunResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let scan_run_id = scan_run_id
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_scan_run_id", "scan run ID 必須是正整數"))?;
    let response = tokio::task::spawn_blocking(move || {
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
        let run = application
            .repository()
            .scan_run(scan_run_id)
            .map_err(ApiError::from_storage)?;
        let issues = application
            .repository()
            .scan_issues(scan_run_id)
            .map_err(ApiError::from_storage)?;
        ScanRunResponse::from_snapshots(run, issues)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(response))
}

#[derive(Debug, Serialize)]
pub(crate) struct ScanRunResponse {
    id: i64,
    status: String,
    started_at: String,
    completed_at: Option<String>,
    summary: Option<Value>,
    error_message: Option<String>,
    issues: Vec<ScanIssueResponse>,
}

impl ScanRunResponse {
    fn from_snapshots(
        run: ScanRunSnapshot,
        issues: Vec<ScanIssueSnapshot>,
    ) -> Result<Self, ApiError> {
        let summary = run
            .summary_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| ApiError::internal())?;
        Ok(Self {
            id: run.id,
            status: run.status.as_str().to_owned(),
            started_at: run.started_at,
            completed_at: run.completed_at,
            summary,
            error_message: run.error_message,
            issues: issues
                .into_iter()
                .map(|issue| ScanIssueResponse {
                    id: issue.id,
                    path: issue.path,
                    kind: issue.kind,
                    message: issue.message,
                })
                .collect(),
        })
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ScanIssueResponse {
    id: i64,
    path: String,
    kind: String,
    message: String,
}
