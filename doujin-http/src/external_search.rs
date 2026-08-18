//! External search job and batch endpoints.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use doujin_app::external_search::{ExternalSearchBatchFieldNeed, ExternalSearchBatchPreflight};
use doujin_files::RecycleBin;
use doujin_storage::external_search_batches::{
    ExternalSearchBatchItemSnapshot, ExternalSearchBatchSnapshot,
};
use doujin_storage::jobs::{
    ExternalSearchActivityItem, ExternalSearchActivitySnapshot, ExternalSearchEnqueueOutcome,
    ExternalSearchJobSnapshot,
};
use doujin_storage::metadata::MetadataField;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ApiError;
use crate::params::{
    parse_collection_id, parse_external_search_batch_id, parse_external_search_batch_strategy,
    parse_external_search_fields, parse_external_search_job_id,
};
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalSearchJobRequest {
    fields: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalSearchBatchRequest {
    collection_ids: Vec<i64>,
    fields: Vec<String>,
    strategy: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchBatchFieldNeedResponse {
    field: &'static str,
    count: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchBatchPreflightItemResponse {
    collection_id: i64,
    fields: Vec<&'static str>,
    outcome: &'static str,
    job_id: Option<i64>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchBatchPreflightResponse {
    strategy: &'static str,
    fields: Vec<&'static str>,
    total: usize,
    will_enqueue: usize,
    reused: usize,
    skipped: usize,
    unchanged: usize,
    insufficient_identifiers: usize,
    field_needs: Vec<ExternalSearchBatchFieldNeedResponse>,
    items: Vec<ExternalSearchBatchPreflightItemResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchBatchSummaryResponse {
    total: usize,
    pending: usize,
    running: usize,
    succeeded: usize,
    partial: usize,
    failed: usize,
    skipped: usize,
    unchanged: usize,
    reused: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchBatchItemResponse {
    collection_id: i64,
    job_id: Option<i64>,
    outcome: &'static str,
    fields: Vec<&'static str>,
    reason: Option<String>,
    status: Option<&'static str>,
    error_kind: Option<String>,
    error_message: Option<String>,
    next_retry_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchBatchResponse {
    id: i64,
    strategy: &'static str,
    fields: Vec<&'static str>,
    created_at: String,
    summary: ExternalSearchBatchSummaryResponse,
    items: Vec<ExternalSearchBatchItemResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchEnqueueResponse {
    created: bool,
    job: ExternalSearchJobResponse,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchJobResponse {
    id: i64,
    collection_id: i64,
    status: &'static str,
    fields: Vec<&'static str>,
    result: Option<Value>,
    error_kind: Option<&'static str>,
    error_message: Option<String>,
    attempts: i64,
    next_retry_at: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchActivityItemResponse {
    #[serde(flatten)]
    job: ExternalSearchJobResponse,
    actionable: bool,
    resolution: Option<&'static str>,
    unresolved_fields: Vec<&'static str>,
    acknowledged_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchActivityResponse {
    actionable_count: usize,
    items: Vec<ExternalSearchActivityItemResponse>,
}

impl TryFrom<ExternalSearchJobSnapshot> for ExternalSearchJobResponse {
    type Error = ApiError;

    fn try_from(job: ExternalSearchJobSnapshot) -> Result<Self, Self::Error> {
        Ok(Self {
            id: job.id,
            collection_id: job.collection_id,
            status: job.status.as_str(),
            fields: job.fields.into_iter().map(MetadataField::as_str).collect(),
            result: job
                .result_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| ApiError::internal())?,
            error_kind: job.error_kind.map(|kind| kind.as_str()),
            error_message: job.error_message,
            attempts: job.attempts,
            next_retry_at: job.next_retry_at,
            created_at: job.created_at,
            updated_at: job.updated_at,
        })
    }
}

impl TryFrom<ExternalSearchEnqueueOutcome> for ExternalSearchEnqueueResponse {
    type Error = ApiError;

    fn try_from(outcome: ExternalSearchEnqueueOutcome) -> Result<Self, Self::Error> {
        Ok(Self {
            created: outcome.created,
            job: outcome.job.try_into()?,
        })
    }
}

impl TryFrom<ExternalSearchActivityItem> for ExternalSearchActivityItemResponse {
    type Error = ApiError;

    fn try_from(item: ExternalSearchActivityItem) -> Result<Self, Self::Error> {
        Ok(Self {
            job: item.job.try_into()?,
            actionable: item.actionable,
            resolution: item.resolution.map(|resolution| resolution.as_str()),
            unresolved_fields: item
                .unresolved_fields
                .into_iter()
                .map(MetadataField::as_str)
                .collect(),
            acknowledged_at: item.acknowledged_at,
        })
    }
}

impl TryFrom<ExternalSearchActivitySnapshot> for ExternalSearchActivityResponse {
    type Error = ApiError;

    fn try_from(activity: ExternalSearchActivitySnapshot) -> Result<Self, Self::Error> {
        Ok(Self {
            actionable_count: activity.actionable_count,
            items: activity
                .items
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

impl From<ExternalSearchBatchFieldNeed> for ExternalSearchBatchFieldNeedResponse {
    fn from(need: ExternalSearchBatchFieldNeed) -> Self {
        Self {
            field: need.field.as_str(),
            count: need.count,
        }
    }
}

impl From<ExternalSearchBatchPreflight> for ExternalSearchBatchPreflightResponse {
    fn from(preflight: ExternalSearchBatchPreflight) -> Self {
        Self {
            strategy: preflight.strategy.as_str(),
            fields: preflight
                .fields
                .into_iter()
                .map(MetadataField::as_str)
                .collect(),
            total: preflight.total,
            will_enqueue: preflight.will_enqueue,
            reused: preflight.reused,
            skipped: preflight.skipped,
            unchanged: preflight.unchanged,
            insufficient_identifiers: preflight.insufficient_identifiers,
            field_needs: preflight.field_needs.into_iter().map(Into::into).collect(),
            items: preflight
                .items
                .into_iter()
                .map(|item| ExternalSearchBatchPreflightItemResponse {
                    collection_id: item.collection_id,
                    fields: item.fields.into_iter().map(MetadataField::as_str).collect(),
                    outcome: item.outcome.as_str(),
                    job_id: item.job_id,
                    reason: item.reason,
                })
                .collect(),
        }
    }
}

impl From<ExternalSearchBatchItemSnapshot> for ExternalSearchBatchItemResponse {
    fn from(item: ExternalSearchBatchItemSnapshot) -> Self {
        Self {
            collection_id: item.collection_id,
            job_id: item.job_id,
            outcome: item.outcome.as_str(),
            fields: item.fields.into_iter().map(MetadataField::as_str).collect(),
            reason: item.reason,
            status: item.job_status.map(|status| status.as_str()),
            error_kind: item.error_kind,
            error_message: item.error_message,
            next_retry_at: item.next_retry_at,
        }
    }
}

impl From<ExternalSearchBatchSnapshot> for ExternalSearchBatchResponse {
    fn from(batch: ExternalSearchBatchSnapshot) -> Self {
        Self {
            id: batch.id,
            strategy: batch.strategy.as_str(),
            fields: batch
                .fields
                .into_iter()
                .map(MetadataField::as_str)
                .collect(),
            created_at: batch.created_at,
            summary: ExternalSearchBatchSummaryResponse {
                total: batch.summary.total,
                pending: batch.summary.pending,
                running: batch.summary.running,
                succeeded: batch.summary.succeeded,
                partial: batch.summary.partial,
                failed: batch.summary.failed,
                skipped: batch.summary.skipped,
                unchanged: batch.summary.unchanged,
                reused: batch.summary.reused,
            },
            items: batch.items.into_iter().map(Into::into).collect(),
        }
    }
}

pub(crate) async fn enqueue_external_search<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    payload: Result<Json<ExternalSearchJobRequest>, JsonRejection>,
) -> Result<Json<ExternalSearchEnqueueResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let fields = parse_external_search_fields(payload.fields)?;
    let outcome = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .enqueue_external_search(collection_id, &fields)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(outcome.try_into()?))
}

pub(crate) async fn get_external_search_job<R>(
    State(state): State<HttpState<R>>,
    Path(job_id): Path<String>,
) -> Result<Json<ExternalSearchJobResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job_id = parse_external_search_job_id(&job_id)?;
    let job = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .external_search_job(job_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(job.try_into()?))
}

pub(crate) async fn get_external_search_activity<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<ExternalSearchActivityResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let activity = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .external_search_activity()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(activity.try_into()?))
}

pub(crate) async fn acknowledge_external_search_job<R>(
    State(state): State<HttpState<R>>,
    Path(job_id): Path<String>,
) -> Result<Json<ExternalSearchActivityItemResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let job_id = parse_external_search_job_id(&job_id)?;
    let item = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .acknowledge_external_search_job(job_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(item.try_into()?))
}

pub(crate) async fn preflight_external_search_batch<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<ExternalSearchBatchRequest>, JsonRejection>,
) -> Result<Json<ExternalSearchBatchPreflightResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let fields = parse_external_search_fields(payload.fields)?;
    let strategy = parse_external_search_batch_strategy(&payload.strategy)?;
    let preflight = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .preflight_external_search_batch(&payload.collection_ids, &fields, strategy)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(preflight.into()))
}

pub(crate) async fn create_external_search_batch<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<ExternalSearchBatchRequest>, JsonRejection>,
) -> Result<Json<ExternalSearchBatchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let fields = parse_external_search_fields(payload.fields)?;
    let strategy = parse_external_search_batch_strategy(&payload.strategy)?;
    let batch = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .create_external_search_batch(&payload.collection_ids, &fields, strategy)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(batch.into()))
}

pub(crate) async fn get_external_search_batch<R>(
    State(state): State<HttpState<R>>,
    Path(batch_id): Path<String>,
) -> Result<Json<ExternalSearchBatchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let batch_id = parse_external_search_batch_id(&batch_id)?;
    let batch = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .external_search_batch(batch_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(batch.into()))
}

pub(crate) async fn retry_external_search_batch<R>(
    State(state): State<HttpState<R>>,
    Path(batch_id): Path<String>,
) -> Result<Json<ExternalSearchBatchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let batch_id = parse_external_search_batch_id(&batch_id)?;
    let batch = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .retry_external_search_batch(batch_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(batch.into()))
}
