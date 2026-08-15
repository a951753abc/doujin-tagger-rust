//! Saved view CRUD endpoints and query mapping.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use doujin_app::SavedViewWithCount;
use doujin_files::RecycleBin;
use doujin_storage::saved_views::{SavedViewLayout, SavedViewQuery};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::params::{
    collection_sort_name, missing_name, parse_collection_query, parse_saved_view_id,
    sort_direction_name, source_name,
};
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedViewMutationRequest {
    name: String,
    query: SavedViewQueryRequest,
    #[serde(default = "default_true")]
    pinned: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedViewQueryRequest {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    classification: Option<String>,
    #[serde(default)]
    missing: Vec<String>,
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    circle: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    parody: Option<String>,
    #[serde(default)]
    subcategory: Option<String>,
    #[serde(default)]
    tag: Vec<String>,
    #[serde(default)]
    untagged: bool,
    sort: String,
    direction: String,
    layout: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SavedViewsResponse {
    items: Vec<SavedViewResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SavedViewResponse {
    id: i64,
    name: String,
    query: SavedViewQueryResponse,
    pinned: bool,
    result_count: i64,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SavedViewQueryResponse {
    q: Option<String>,
    source: Option<&'static str>,
    classification: Option<String>,
    missing: Vec<&'static str>,
    event: Option<String>,
    circle: Option<String>,
    author: Option<String>,
    parody: Option<String>,
    subcategory: Option<String>,
    tag: Vec<String>,
    untagged: bool,
    sort: &'static str,
    direction: &'static str,
    layout: &'static str,
}

impl From<SavedViewWithCount> for SavedViewResponse {
    fn from(saved: SavedViewWithCount) -> Self {
        let view = saved.view;
        Self {
            id: view.id,
            name: view.name,
            query: saved_view_query_response(view.query),
            pinned: view.pinned,
            result_count: saved.result_count,
            created_at: view.created_at,
            updated_at: view.updated_at,
        }
    }
}

pub(crate) async fn list_saved_views<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<SavedViewsResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let items = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .saved_views()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(SavedViewsResponse {
        items: items.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn get_saved_view<R>(
    State(state): State<HttpState<R>>,
    Path(saved_view_id): Path<String>,
) -> Result<Json<SavedViewResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let saved_view_id = parse_saved_view_id(&saved_view_id)?;
    let saved = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .saved_view(saved_view_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(saved.into()))
}

pub(crate) async fn create_saved_view<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<SavedViewMutationRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<SavedViewResponse>), ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_saved_view", "Saved View JSON 無效"))?;
    let query = saved_view_query(payload.query)?;
    let saved = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .create_saved_view(&payload.name, &query, payload.pinned)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok((StatusCode::CREATED, Json(saved.into())))
}

pub(crate) async fn update_saved_view<R>(
    State(state): State<HttpState<R>>,
    Path(saved_view_id): Path<String>,
    payload: Result<Json<SavedViewMutationRequest>, JsonRejection>,
) -> Result<Json<SavedViewResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let saved_view_id = parse_saved_view_id(&saved_view_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_saved_view", "Saved View JSON 無效"))?;
    let query = saved_view_query(payload.query)?;
    let saved = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .update_saved_view(saved_view_id, &payload.name, &query, payload.pinned)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(saved.into()))
}

pub(crate) async fn delete_saved_view<R>(
    State(state): State<HttpState<R>>,
    Path(saved_view_id): Path<String>,
) -> Result<StatusCode, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let saved_view_id = parse_saved_view_id(&saved_view_id)?;
    tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .delete_saved_view(saved_view_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn saved_view_query(request: SavedViewQueryRequest) -> Result<SavedViewQuery, ApiError> {
    if !matches!(request.sort.as_str(), "created" | "updated" | "title") {
        return Err(ApiError::bad_request(
            "invalid_saved_view",
            "sort 必須是 created、updated 或 title",
        ));
    }
    if !matches!(request.direction.as_str(), "asc" | "desc") {
        return Err(ApiError::bad_request(
            "invalid_saved_view",
            "direction 必須是 asc 或 desc",
        ));
    }
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    append_optional_parameter(&mut serializer, "q", request.q.as_deref());
    append_optional_parameter(&mut serializer, "source", request.source.as_deref());
    append_optional_parameter(
        &mut serializer,
        "classification",
        request.classification.as_deref(),
    );
    for missing in &request.missing {
        serializer.append_pair("missing", missing);
    }
    append_optional_parameter(&mut serializer, "event", request.event.as_deref());
    append_optional_parameter(&mut serializer, "circle", request.circle.as_deref());
    append_optional_parameter(&mut serializer, "author", request.author.as_deref());
    append_optional_parameter(&mut serializer, "parody", request.parody.as_deref());
    append_optional_parameter(
        &mut serializer,
        "subcategory",
        request.subcategory.as_deref(),
    );
    for tag in &request.tag {
        serializer.append_pair("tag", tag);
    }
    if request.untagged {
        serializer.append_pair("untagged", "1");
    }
    serializer.append_pair("sort", &request.sort);
    serializer.append_pair("direction", &request.direction);
    let raw_query = serializer.finish();
    let query = parse_collection_query(Some(&raw_query))?;
    let layout = match request.layout.as_str() {
        "list" => SavedViewLayout::List,
        "grid" => SavedViewLayout::Grid,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_saved_view",
                "layout 必須是 list 或 grid",
            ));
        }
    };
    Ok(SavedViewQuery::from_collection_query(&query, layout))
}

pub(crate) fn append_optional_parameter(
    serializer: &mut form_urlencoded::Serializer<'_, String>,
    name: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        serializer.append_pair(name, value);
    }
}

pub(crate) fn saved_view_query_response(query: SavedViewQuery) -> SavedViewQueryResponse {
    SavedViewQueryResponse {
        q: query.search,
        source: query.filters.source.map(source_name),
        classification: query.filters.classification,
        missing: query
            .filters
            .missing
            .into_iter()
            .map(missing_name)
            .collect(),
        event: query.filters.event,
        circle: query.filters.circle,
        author: query.filters.author,
        parody: query.filters.parody,
        subcategory: query.filters.subcategory,
        tag: query.filters.tags,
        untagged: query.filters.untagged,
        sort: collection_sort_name(query.sort),
        direction: sort_direction_name(query.direction),
        layout: query.layout.as_str(),
    }
}

pub(crate) fn default_true() -> bool {
    true
}
