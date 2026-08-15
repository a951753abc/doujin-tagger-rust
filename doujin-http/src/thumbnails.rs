//! Thumbnail delivery, rebuild and cache job endpoints.

use std::sync::TryLockError;
use std::time::Instant;

use axum::Json;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use doujin_app::{ApplicationError, ApplicationService};
use doujin_files::RecycleBin;
use doujin_storage::StorageError;
use doujin_storage::thumbnails::{
    DEFAULT_THUMBNAIL_PRIORITY, MAX_THUMBNAIL_PRIORITY, ThumbnailStateSnapshot, ThumbnailStatus,
};
use doujin_thumbnails::transparent_placeholder_webp;
use serde::{Deserialize, Serialize};

use crate::collections::CollectionResponse;
use crate::error::ApiError;
use crate::params::parse_collection_id;
use crate::{HttpState, ThumbnailCacheJob, lock_interactive_application};

pub(crate) async fn get_thumbnail<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let priority = parse_thumbnail_priority(raw_query.as_deref())?;
    let (thumbnail, cache) = tokio::task::spawn_blocking(move || {
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
        let outcome = application
            .request_thumbnail_with_priority(collection_id, priority)
            .map_err(ApiError::from_application)?;
        let cache = application
            .read_thumbnail_cache(collection_id)
            .map_err(ApiError::from_application)?;
        Ok((outcome.state, cache))
    })
    .await
    .map_err(|_| ApiError::internal())??;

    let ready = thumbnail.status == ThumbnailStatus::Ready && cache.is_some();
    let body = cache.unwrap_or_else(|| transparent_placeholder_webp().to_vec());
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = if ready {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("image/webp"));
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static(if ready {
            "private, max-age=86400"
        } else {
            "no-store"
        }),
    );
    response.headers_mut().insert(
        "x-thumbnail-status",
        HeaderValue::from_static(thumbnail.status.as_str()),
    );
    response.headers_mut().insert(
        "x-thumbnail-priority",
        HeaderValue::from_str(&thumbnail.priority.to_string()).map_err(|_| ApiError::internal())?,
    );
    if let Some(error_kind) = thumbnail.error_kind {
        response.headers_mut().insert(
            "x-thumbnail-error-kind",
            HeaderValue::from_static(error_kind.as_str()),
        );
    }
    if let Some(next_retry_at) = thumbnail.next_retry_at {
        response.headers_mut().insert(
            "x-thumbnail-next-retry-at",
            HeaderValue::from_str(&next_retry_at).map_err(|_| ApiError::internal())?,
        );
    }
    Ok(response)
}

#[derive(Debug, Serialize)]
pub(crate) struct ThumbnailStateResponse {
    collection_id: i64,
    status: &'static str,
    error_kind: Option<&'static str>,
    error_message: Option<String>,
    attempts: i64,
    next_retry_at: Option<String>,
    generated_width: Option<u32>,
    generated_height: Option<u32>,
    priority: i64,
    requested_at: Option<String>,
}

impl From<ThumbnailStateSnapshot> for ThumbnailStateResponse {
    fn from(state: ThumbnailStateSnapshot) -> Self {
        Self {
            collection_id: state.collection_id,
            status: state.status.as_str(),
            error_kind: state.error_kind.map(|kind| kind.as_str()),
            error_message: state.error_message,
            attempts: state.attempts,
            next_retry_at: state.next_retry_at,
            generated_width: state.generated_width,
            generated_height: state.generated_height,
            priority: state.priority,
            requested_at: state.requested_at,
        }
    }
}

pub(crate) fn parse_thumbnail_priority(raw_query: Option<&str>) -> Result<i64, ApiError> {
    let mut priority = DEFAULT_THUMBNAIL_PRIORITY;
    let mut seen = false;
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        if key != "priority" || seen {
            return Err(ApiError::bad_request(
                "invalid_thumbnail_priority",
                "thumbnail 僅接受一個 priority 參數",
            ));
        }
        priority = value.parse::<i64>().map_err(|_| {
            ApiError::bad_request(
                "invalid_thumbnail_priority",
                "thumbnail priority 必須是正整數",
            )
        })?;
        if !(DEFAULT_THUMBNAIL_PRIORITY..=MAX_THUMBNAIL_PRIORITY).contains(&priority) {
            return Err(ApiError::bad_request(
                "invalid_thumbnail_priority",
                "thumbnail priority 超出允許範圍",
            ));
        }
        seen = true;
    }
    Ok(priority)
}

pub(crate) async fn rebuild_thumbnail<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
) -> Result<Json<ThumbnailStateResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let thumbnail = tokio::task::spawn_blocking(move || {
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
            .rebuild_thumbnail(collection_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(thumbnail.into()))
}

#[derive(Debug, Serialize)]
pub(crate) struct ThumbnailRebuildAllResponse {
    rebuilt: usize,
}

pub(crate) async fn rebuild_all_thumbnails<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<ThumbnailRebuildAllResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let rebuilt = tokio::task::spawn_blocking(move || {
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
            .rebuild_all_thumbnails()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(ThumbnailRebuildAllResponse { rebuilt }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThumbnailCacheJobRequest {
    root_ids: Vec<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ThumbnailCachePreflightResponse {
    root_ids: Vec<i64>,
    root_count: usize,
    collection_count: usize,
    ready: usize,
    requires_build: usize,
    known_failures: usize,
    cancellation_supported: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct ThumbnailCacheJobEnvelope {
    job: Option<ThumbnailCacheJobResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ThumbnailCacheJobResponse {
    id: u64,
    root_ids: Vec<i64>,
    status: &'static str,
    total: usize,
    pending: usize,
    running: usize,
    ready: usize,
    failed: usize,
    failed_collection_ids: Vec<i64>,
    progress_percent: f64,
    elapsed_seconds: u64,
    estimated_seconds_remaining: Option<u64>,
}

impl ThumbnailCacheJobResponse {
    fn is_running(&self) -> bool {
        self.status == "running"
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ThumbnailCacheFailuresResponse {
    job_id: Option<u64>,
    items: Vec<CollectionResponse>,
    missing_collection_ids: Vec<i64>,
}

pub(crate) async fn preflight_thumbnail_cache_job<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<ThumbnailCacheJobRequest>, JsonRejection>,
) -> Result<Json<ThumbnailCachePreflightResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let response = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        let preflight = application
            .thumbnail_cache_preflight(&payload.root_ids)
            .map_err(ApiError::from_application)?;
        let collection_count = preflight.collection_ids.len();
        Ok::<_, ApiError>(ThumbnailCachePreflightResponse {
            root_count: preflight.root_ids.len(),
            root_ids: preflight.root_ids,
            collection_count,
            ready: preflight.ready,
            requires_build: collection_count.saturating_sub(preflight.ready),
            known_failures: application
                .thumbnail_failed_collection_ids(&preflight.collection_ids)
                .map_err(ApiError::from_application)?
                .len(),
            cancellation_supported: false,
        })
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(response))
}

pub(crate) async fn start_thumbnail_cache_job<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<ThumbnailCacheJobRequest>, JsonRejection>,
) -> Result<Json<ThumbnailCacheJobResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let response = tokio::task::spawn_blocking(move || {
        let mut jobs = state
            .thumbnail_cache_jobs
            .lock()
            .map_err(|_| ApiError::internal())?;
        let mut application = lock_interactive_application(&state.application)?;
        if let Some(current) = jobs.current.as_ref() {
            let response = thumbnail_cache_job_response(&application, current)?;
            if response.is_running() {
                return Err(ApiError::conflict(
                    "thumbnail_cache_job_running",
                    "已有快取縮圖工作正在進行",
                ));
            }
        }

        let mut root_ids = payload.root_ids;
        root_ids.sort_unstable();
        root_ids.dedup();
        let prepared = application
            .prepare_thumbnail_cache(&root_ids)
            .map_err(ApiError::from_application)?;
        let counts = application
            .thumbnail_status_counts(&prepared.collection_ids)
            .map_err(ApiError::from_application)?;
        let initial_completed = counts
            .ready
            .saturating_add(counts.failed)
            .saturating_add(counts.missing)
            .saturating_add(prepared.failed_collection_ids.len());
        jobs.next_id = jobs.next_id.saturating_add(1);
        let job = ThumbnailCacheJob {
            id: jobs.next_id,
            root_ids,
            collection_ids: prepared.collection_ids,
            failed_collection_ids: prepared.failed_collection_ids,
            initial_completed,
            started_at: Instant::now(),
        };
        let response = thumbnail_cache_job_response(&application, &job)?;
        jobs.current = Some(job);
        Ok(response)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(response))
}

pub(crate) async fn get_current_thumbnail_cache_job<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<ThumbnailCacheJobEnvelope>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job = tokio::task::spawn_blocking(move || {
        let jobs = state
            .thumbnail_cache_jobs
            .lock()
            .map_err(|_| ApiError::internal())?;
        let Some(current) = jobs.current.as_ref() else {
            return Ok(None);
        };
        let application = lock_interactive_application(&state.application)?;
        thumbnail_cache_job_response(&application, current).map(Some)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(ThumbnailCacheJobEnvelope { job }))
}

pub(crate) async fn get_thumbnail_cache_failures<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<ThumbnailCacheFailuresResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let response = tokio::task::spawn_blocking(move || {
        let jobs = state
            .thumbnail_cache_jobs
            .lock()
            .map_err(|_| ApiError::internal())?;
        let Some(job) = jobs.current.as_ref() else {
            return Ok(ThumbnailCacheFailuresResponse {
                job_id: None,
                items: Vec::new(),
                missing_collection_ids: Vec::new(),
            });
        };
        let application = lock_interactive_application(&state.application)?;
        let failed_collection_ids = thumbnail_cache_failed_ids(&application, job)?;
        let mut items = Vec::with_capacity(failed_collection_ids.len());
        let mut missing_collection_ids = Vec::new();
        for collection_id in failed_collection_ids {
            match application.collection(collection_id) {
                Ok(collection) => items.push(collection.into()),
                Err(ApplicationError::Storage(StorageError::CollectionNotFound(_))) => {
                    missing_collection_ids.push(collection_id);
                }
                Err(error) => return Err(ApiError::from_application(error)),
            }
        }
        Ok(ThumbnailCacheFailuresResponse {
            job_id: Some(job.id),
            items,
            missing_collection_ids,
        })
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(response))
}

pub(crate) async fn retry_thumbnail_cache_failures<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<ThumbnailCacheJobResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let response = tokio::task::spawn_blocking(move || {
        let mut jobs = state
            .thumbnail_cache_jobs
            .lock()
            .map_err(|_| ApiError::internal())?;
        let mut application = lock_interactive_application(&state.application)?;
        let current = jobs.current.as_ref().ok_or_else(|| {
            ApiError::conflict(
                "thumbnail_cache_job_not_found",
                "目前沒有可重試的快取縮圖工作",
            )
        })?;
        let current_response = thumbnail_cache_job_response(&application, current)?;
        if current_response.is_running() {
            return Err(ApiError::conflict(
                "thumbnail_cache_job_running",
                "快取縮圖工作仍在進行，完成後才能重試失敗項目",
            ));
        }
        let failure_ids = current_response.failed_collection_ids;
        if failure_ids.is_empty() {
            return Err(ApiError::conflict(
                "thumbnail_cache_no_failures",
                "目前的快取縮圖工作沒有失敗項目",
            ));
        }
        let root_ids = current.root_ids.clone();
        let prepared = application
            .retry_thumbnails(&failure_ids)
            .map_err(ApiError::from_application)?;
        let counts = application
            .thumbnail_status_counts(&prepared.collection_ids)
            .map_err(ApiError::from_application)?;
        let initial_completed = counts
            .ready
            .saturating_add(counts.failed)
            .saturating_add(counts.missing)
            .saturating_add(prepared.failed_collection_ids.len());
        jobs.next_id = jobs.next_id.saturating_add(1);
        let job = ThumbnailCacheJob {
            id: jobs.next_id,
            root_ids,
            collection_ids: prepared.collection_ids,
            failed_collection_ids: prepared.failed_collection_ids,
            initial_completed,
            started_at: Instant::now(),
        };
        let response = thumbnail_cache_job_response(&application, &job)?;
        jobs.current = Some(job);
        Ok(response)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(response))
}

pub(crate) fn thumbnail_cache_failed_ids<R: RecycleBin>(
    application: &ApplicationService<R>,
    job: &ThumbnailCacheJob,
) -> Result<Vec<i64>, ApiError> {
    let mut failed_collection_ids = application
        .thumbnail_failed_collection_ids(&job.collection_ids)
        .map_err(ApiError::from_application)?;
    failed_collection_ids.extend(job.failed_collection_ids.iter().copied());
    failed_collection_ids.sort_unstable();
    failed_collection_ids.dedup();
    Ok(failed_collection_ids)
}

pub(crate) fn thumbnail_cache_job_response<R: RecycleBin>(
    application: &ApplicationService<R>,
    job: &ThumbnailCacheJob,
) -> Result<ThumbnailCacheJobResponse, ApiError> {
    let counts = application
        .thumbnail_status_counts(&job.collection_ids)
        .map_err(ApiError::from_application)?;
    let failed_collection_ids = thumbnail_cache_failed_ids(application, job)?;
    let failed = failed_collection_ids.len();
    let total = job
        .collection_ids
        .len()
        .saturating_add(job.failed_collection_ids.len());
    let completed = counts.ready.saturating_add(failed).min(total);
    let status = if completed < total {
        "running"
    } else if failed > 0 {
        "completed_with_errors"
    } else {
        "completed"
    };
    let progress_percent = if total == 0 {
        100.0
    } else {
        ((completed as f64 / total as f64) * 1_000.0).round() / 10.0
    };
    let elapsed = job.started_at.elapsed();
    let newly_completed = completed.saturating_sub(job.initial_completed);
    let estimated_seconds_remaining = if completed >= total {
        Some(0)
    } else if newly_completed == 0 {
        None
    } else {
        let seconds_per_item = elapsed.as_secs_f64() / newly_completed as f64;
        Some((seconds_per_item * total.saturating_sub(completed) as f64).ceil() as u64)
    };
    Ok(ThumbnailCacheJobResponse {
        id: job.id,
        root_ids: job.root_ids.clone(),
        status,
        total,
        pending: counts.pending,
        running: counts.running,
        ready: counts.ready,
        failed,
        failed_collection_ids,
        progress_percent,
        elapsed_seconds: elapsed.as_secs(),
        estimated_seconds_remaining,
    })
}
