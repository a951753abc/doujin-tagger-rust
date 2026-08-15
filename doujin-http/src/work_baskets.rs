//! Work basket endpoints.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use doujin_files::RecycleBin;
use doujin_storage::work_baskets::{WorkBasketItemSnapshot, WorkBasketSnapshot, WorkBasketSummary};
use serde::{Deserialize, Serialize};

use crate::collections::CollectionResponse;
use crate::error::ApiError;
use crate::metadata::validate_batch_collection_ids;
use crate::params::{parse_collection_id, parse_work_basket_id};
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Serialize)]
pub(crate) struct WorkBasketsResponse {
    baskets: Vec<WorkBasketSummaryResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorkBasketSummaryResponse {
    id: i64,
    name: String,
    count: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorkBasketResponse {
    id: i64,
    name: String,
    count: usize,
    items: Vec<WorkBasketItemResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorkBasketItemResponse {
    collection: CollectionResponse,
    added_at: String,
}

impl From<WorkBasketSummary> for WorkBasketSummaryResponse {
    fn from(basket: WorkBasketSummary) -> Self {
        Self {
            id: basket.id,
            name: basket.name,
            count: basket.count,
        }
    }
}

impl From<WorkBasketItemSnapshot> for WorkBasketItemResponse {
    fn from(item: WorkBasketItemSnapshot) -> Self {
        Self {
            collection: item.collection.into(),
            added_at: item.added_at,
        }
    }
}

impl From<WorkBasketSnapshot> for WorkBasketResponse {
    fn from(basket: WorkBasketSnapshot) -> Self {
        let count = basket.items.len();
        Self {
            id: basket.id,
            name: basket.name,
            count,
            items: basket.items.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkBasketCollectionsRequest {
    collection_ids: Vec<i64>,
}

pub(crate) async fn list_work_baskets<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<WorkBasketsResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let baskets = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .work_baskets()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(WorkBasketsResponse {
        baskets: baskets.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn get_work_basket<R>(
    State(state): State<HttpState<R>>,
    Path(basket_id): Path<String>,
) -> Result<Json<WorkBasketResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let basket_id = parse_work_basket_id(&basket_id)?;
    let basket = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .work_basket(basket_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(basket.into()))
}

pub(crate) async fn add_work_basket_collections<R>(
    State(state): State<HttpState<R>>,
    Path(basket_id): Path<String>,
    payload: Result<Json<WorkBasketCollectionsRequest>, JsonRejection>,
) -> Result<Json<WorkBasketResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let basket_id = parse_work_basket_id(&basket_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validate_batch_collection_ids(payload.collection_ids)?;
    let basket = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .add_to_work_basket(basket_id, &collection_ids)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(basket.into()))
}

pub(crate) async fn remove_work_basket_collection<R>(
    State(state): State<HttpState<R>>,
    Path((basket_id, collection_id)): Path<(String, String)>,
) -> Result<Json<WorkBasketResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let basket_id = parse_work_basket_id(&basket_id)?;
    let collection_id = parse_collection_id(&collection_id)?;
    let basket = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .remove_from_work_basket(basket_id, collection_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(basket.into()))
}

pub(crate) async fn clear_work_basket<R>(
    State(state): State<HttpState<R>>,
    Path(basket_id): Path<String>,
) -> Result<Json<WorkBasketResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let basket_id = parse_work_basket_id(&basket_id)?;
    let basket = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .clear_work_basket(basket_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(basket.into()))
}
