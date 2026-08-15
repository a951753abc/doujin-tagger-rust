//! Duplicate scan jobs and duplicate pair decisions.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, RawQuery, State};
use doujin_files::RecycleBin;
use doujin_storage::duplicates::{
    DuplicateCandidatePair, DuplicateCollectionEvidence, DuplicateScanItemSnapshot,
    DuplicateScanJobSnapshot,
};
use serde::{Deserialize, Serialize};

use crate::collections::CollectionResponse;
use crate::error::ApiError;
use crate::params::parse_collection_id;
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Serialize)]
pub(crate) struct DuplicateScanJobResponse {
    id: i64,
    status: &'static str,
    total: usize,
    pending: usize,
    running: usize,
    processed: usize,
    failed: usize,
    reused_cache: usize,
    concurrency_limit: usize,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
    estimated_seconds_remaining: Option<u64>,
}

impl From<DuplicateScanJobSnapshot> for DuplicateScanJobResponse {
    fn from(job: DuplicateScanJobSnapshot) -> Self {
        Self {
            id: job.id,
            status: job.status.as_str(),
            total: job.total,
            pending: job.pending,
            running: job.running,
            processed: job.processed,
            failed: job.failed,
            reused_cache: job.reused_cache,
            concurrency_limit: job.concurrency_limit,
            created_at: job.created_at,
            updated_at: job.updated_at,
            completed_at: job.completed_at,
            // Fingerprinting duration depends on archive size/compression and is
            // too variable for a trustworthy estimate.
            estimated_seconds_remaining: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct DuplicateCandidatesResponse {
    items: Vec<DuplicateCandidateResponse>,
    total: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct DuplicateCandidateResponse {
    left: DuplicateEvidenceResponse,
    right: DuplicateEvidenceResponse,
    level: &'static str,
    confidence: f64,
    reasons: Vec<String>,
    matching_pages: usize,
    compared_pages: usize,
    reviewed: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct DuplicateEvidenceResponse {
    collection: CollectionResponse,
    file_size: u64,
    page_count: usize,
    archive_entry_count: usize,
    fingerprint_identity: String,
    metadata_completeness: usize,
    tag_count: usize,
    manual_assertion_count: usize,
    identifiers: Vec<String>,
    max_image_width: Option<u32>,
    max_image_height: Option<u32>,
}

impl From<DuplicateCollectionEvidence> for DuplicateEvidenceResponse {
    fn from(evidence: DuplicateCollectionEvidence) -> Self {
        Self {
            collection: evidence.collection.into(),
            file_size: evidence.file_size,
            page_count: evidence.page_count,
            archive_entry_count: evidence.archive_entry_count,
            fingerprint_identity: evidence.fingerprint_identity,
            metadata_completeness: evidence.metadata_completeness,
            tag_count: evidence.tag_count,
            manual_assertion_count: evidence.manual_assertion_count,
            identifiers: evidence.identifiers,
            max_image_width: evidence.max_image_width,
            max_image_height: evidence.max_image_height,
        }
    }
}

impl From<DuplicateCandidatePair> for DuplicateCandidateResponse {
    fn from(candidate: DuplicateCandidatePair) -> Self {
        Self {
            left: candidate.left.into(),
            right: candidate.right.into(),
            level: candidate.level.as_str(),
            confidence: candidate.confidence,
            reasons: candidate.reasons,
            matching_pages: candidate.matching_pages,
            compared_pages: candidate.compared_pages,
            reviewed: candidate.reviewed,
        }
    }
}

pub(crate) async fn start_duplicate_scan<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<DuplicateScanJobResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job = tokio::task::spawn_blocking(move || {
        lock_interactive_application(&state.application)?
            .start_duplicate_scan()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(job.into()))
}

pub(crate) async fn get_duplicate_scan<R>(
    State(state): State<HttpState<R>>,
    Path(job_id): Path<String>,
) -> Result<Json<DuplicateScanJobResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job_id = parse_positive_id(&job_id, "duplicate scan job ID")?;
    let job = tokio::task::spawn_blocking(move || {
        lock_interactive_application(&state.application)?
            .duplicate_scan_job(job_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(job.into()))
}

#[derive(Debug, Serialize)]
pub(crate) struct DuplicateScanJobEnvelope {
    job: Option<DuplicateScanJobResponse>,
}

pub(crate) async fn get_current_duplicate_scan<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<DuplicateScanJobEnvelope>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job = tokio::task::spawn_blocking(move || {
        lock_interactive_application(&state.application)?
            .latest_duplicate_scan_job()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(DuplicateScanJobEnvelope {
        job: job.map(Into::into),
    }))
}

#[derive(Debug, Serialize)]
pub(crate) struct DuplicateScanFailuresResponse {
    items: Vec<DuplicateScanFailureResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DuplicateScanFailureResponse {
    collection_id: i64,
    path: String,
    error_kind: Option<String>,
    error_message: Option<String>,
    attempts: usize,
}

impl From<DuplicateScanItemSnapshot> for DuplicateScanFailureResponse {
    fn from(item: DuplicateScanItemSnapshot) -> Self {
        Self {
            collection_id: item.collection_id,
            path: item.path.to_string_lossy().into_owned(),
            error_kind: item.error_kind,
            error_message: item.error_message,
            attempts: item.attempts,
        }
    }
}

pub(crate) async fn get_duplicate_scan_failures<R>(
    State(state): State<HttpState<R>>,
    Path(job_id): Path<String>,
) -> Result<Json<DuplicateScanFailuresResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job_id = parse_positive_id(&job_id, "duplicate scan job ID")?;
    let items = tokio::task::spawn_blocking(move || {
        lock_interactive_application(&state.application)?
            .duplicate_scan_failures(job_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(DuplicateScanFailuresResponse {
        items: items.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn retry_duplicate_scan_failures<R>(
    State(state): State<HttpState<R>>,
    Path(job_id): Path<String>,
) -> Result<Json<DuplicateScanJobResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job_id = parse_positive_id(&job_id, "duplicate scan job ID")?;
    let job = tokio::task::spawn_blocking(move || {
        lock_interactive_application(&state.application)?
            .retry_duplicate_scan_failures(job_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(job.into()))
}

pub(crate) async fn list_duplicate_candidates<R>(
    State(state): State<HttpState<R>>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<DuplicateCandidatesResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let level = raw_query
        .as_deref()
        .and_then(|query| form_urlencoded::parse(query.as_bytes()).find(|(key, _)| key == "level"))
        .map(|(_, value)| value.into_owned());
    if level
        .as_deref()
        .is_some_and(|level| !matches!(level, "exact" | "content" | "probable"))
    {
        return Err(ApiError::bad_request(
            "invalid_duplicate_level",
            "duplicate level 必須是 exact、content 或 probable",
        ));
    }
    let candidates = tokio::task::spawn_blocking(move || {
        lock_interactive_application(&state.application)?
            .duplicate_candidates()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    let items = candidates
        .into_iter()
        .filter(|candidate| {
            level
                .as_deref()
                .is_none_or(|level| candidate.level.as_str() == level)
        })
        .map(Into::into)
        .collect::<Vec<_>>();
    Ok(Json(DuplicateCandidatesResponse {
        total: items.len(),
        items,
    }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct DuplicateDecisionRequest {
    left_fingerprint_identity: String,
    right_fingerprint_identity: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DuplicateDecisionResponse {
    status: &'static str,
}

pub(crate) async fn exclude_duplicate_pair<R>(
    State(state): State<HttpState<R>>,
    Path((left_collection_id, right_collection_id)): Path<(String, String)>,
    payload: Result<Json<DuplicateDecisionRequest>, JsonRejection>,
) -> Result<Json<DuplicateDecisionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    decide_duplicate_pair(
        state,
        left_collection_id,
        right_collection_id,
        payload,
        false,
    )
    .await
}

pub(crate) async fn confirm_duplicate_pair<R>(
    State(state): State<HttpState<R>>,
    Path((left_collection_id, right_collection_id)): Path<(String, String)>,
    payload: Result<Json<DuplicateDecisionRequest>, JsonRejection>,
) -> Result<Json<DuplicateDecisionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    decide_duplicate_pair(
        state,
        left_collection_id,
        right_collection_id,
        payload,
        true,
    )
    .await
}

pub(crate) async fn decide_duplicate_pair<R>(
    state: HttpState<R>,
    left_collection_id: String,
    right_collection_id: String,
    payload: Result<Json<DuplicateDecisionRequest>, JsonRejection>,
    confirm: bool,
) -> Result<Json<DuplicateDecisionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let left_collection_id = parse_collection_id(&left_collection_id)?;
    let right_collection_id = parse_collection_id(&right_collection_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        if confirm {
            application.confirm_duplicate_pair(
                left_collection_id,
                &payload.left_fingerprint_identity,
                right_collection_id,
                &payload.right_fingerprint_identity,
            )
        } else {
            application.exclude_duplicate_pair(
                left_collection_id,
                &payload.left_fingerprint_identity,
                right_collection_id,
                &payload.right_fingerprint_identity,
            )
        }
        .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(DuplicateDecisionResponse {
        status: if confirm { "confirmed" } else { "excluded" },
    }))
}

pub(crate) fn parse_positive_id(value: &str, label: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_id", &format!("{label} 必須是正整數")))
}
