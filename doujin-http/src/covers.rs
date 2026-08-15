//! Cover candidate and cover selection endpoints.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use doujin_files::RecycleBin;
use serde::Deserialize;

use crate::error::ApiError;
use crate::params::parse_collection_id;
use crate::{HttpState, lock_interactive_application};

pub(crate) async fn get_cover_candidates<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<doujin_app::ApplicationCoverCandidates>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let limit = parse_cover_candidate_limit(raw_query.as_deref())?;
    let candidates = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .cover_candidates(collection_id, limit)
            .map_err(ApiError::from_cover_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(candidates))
}

pub(crate) fn parse_cover_candidate_limit(raw_query: Option<&str>) -> Result<usize, ApiError> {
    let mut limit = doujin_thumbnails::MAX_COVER_CANDIDATES;
    let mut seen = false;
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        if key != "limit" || seen {
            return Err(ApiError::bad_request(
                "invalid_cover_candidate_limit",
                "封面候選只接受一個 limit 參數",
            ));
        }
        limit = value.parse::<usize>().map_err(|_| {
            ApiError::bad_request(
                "invalid_cover_candidate_limit",
                "封面候選 limit 必須是正整數",
            )
        })?;
        seen = true;
    }
    Ok(limit.clamp(1, doujin_thumbnails::MAX_COVER_CANDIDATES))
}

pub(crate) async fn get_cover_candidate_preview<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let entry_path = parse_cover_entry_query(raw_query.as_deref())?;
    let preview = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .cover_candidate_preview(collection_id, &entry_path)
            .map_err(ApiError::from_cover_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    let mut response = Response::new(Body::from(preview));
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("image/webp"));
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("private, max-age=300"),
    );
    Ok(response)
}

pub(crate) fn parse_cover_entry_query(raw_query: Option<&str>) -> Result<String, ApiError> {
    let mut entry = None;
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        if key != "entry" || entry.is_some() || value.is_empty() {
            return Err(ApiError::bad_request(
                "invalid_cover_entry",
                "候選封面預覽需要單一 entry 參數",
            ));
        }
        entry = Some(value.into_owned());
    }
    entry.ok_or_else(|| ApiError::bad_request("invalid_cover_entry", "候選封面預覽需要 entry 參數"))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CoverSelectionPayload {
    entry_path: String,
    source_fingerprint: String,
}

pub(crate) async fn put_cover_selection<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    Json(payload): Json<CoverSelectionPayload>,
) -> Result<Json<doujin_app::ApplicationCoverSelection>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let selection = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .select_cover(
                collection_id,
                &payload.entry_path,
                &payload.source_fingerprint,
            )
            .map_err(ApiError::from_cover_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(selection))
}

pub(crate) async fn delete_cover_selection<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
) -> Result<StatusCode, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .clear_cover_selection(collection_id)
            .map_err(ApiError::from_cover_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(StatusCode::NO_CONTENT)
}
