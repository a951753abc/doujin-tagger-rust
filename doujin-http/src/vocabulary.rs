//! Vocabulary candidate and merge endpoints.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{RawQuery, State};
use doujin_files::RecycleBin;
use doujin_storage::vocabulary::{
    VocabularyCandidateGroup, VocabularyMergePreflight, VocabularyMergeResult,
};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::params::{parse_vocabulary_field, parse_vocabulary_query};
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Serialize)]
pub(crate) struct VocabularyCandidatesResponse {
    groups: Vec<VocabularyCandidateGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VocabularyMergeRequest {
    field: String,
    canonical: String,
    variants: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VocabularyRejectRequest {
    field: String,
    values: Vec<String>,
    reason: String,
    #[serde(default)]
    removed: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct VocabularyRejectResponse {
    exclusions_recorded: usize,
}

pub(crate) async fn list_vocabulary_candidates<R>(
    State(state): State<HttpState<R>>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<VocabularyCandidatesResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let field = parse_vocabulary_query(raw_query.as_deref())?;
    let groups = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .vocabulary_candidates(field)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(VocabularyCandidatesResponse { groups }))
}

pub(crate) async fn preflight_vocabulary_merge<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<VocabularyMergeRequest>, JsonRejection>,
) -> Result<Json<VocabularyMergePreflight>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let request = payload
        .map_err(|_| ApiError::bad_request("invalid_vocabulary_request", "名稱治理 JSON 無效"))?
        .0;
    let field = parse_vocabulary_field(&request.field)?;
    let preflight = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .vocabulary_merge_preflight(field, &request.canonical, &request.variants)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(preflight))
}

pub(crate) async fn merge_vocabulary<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<VocabularyMergeRequest>, JsonRejection>,
) -> Result<Json<VocabularyMergeResult>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let request = payload
        .map_err(|_| ApiError::bad_request("invalid_vocabulary_request", "名稱治理 JSON 無效"))?
        .0;
    let field = parse_vocabulary_field(&request.field)?;
    let result = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .merge_vocabulary(field, &request.canonical, &request.variants)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(result))
}

pub(crate) async fn reject_vocabulary<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<VocabularyRejectRequest>, JsonRejection>,
) -> Result<Json<VocabularyRejectResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let request = payload
        .map_err(|_| ApiError::bad_request("invalid_vocabulary_request", "名稱治理 JSON 無效"))?
        .0;
    let field = parse_vocabulary_field(&request.field)?;
    let exclusions_recorded = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .reject_vocabulary_group(field, &request.values, &request.reason, request.removed)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(VocabularyRejectResponse {
        exclusions_recorded,
    }))
}
