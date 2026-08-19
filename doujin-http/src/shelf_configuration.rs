//! Shelf composition read, replace, and reset endpoints.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use doujin_files::RecycleBin;
use doujin_storage::shelf_composition::{ShelfConfigurationItem, ShelfType};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShelfConfigurationUpdateRequest {
    items: Vec<ShelfConfigurationItemRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShelfConfigurationItemRequest {
    shelf_type: String,
    saved_view_id: Option<i64>,
    position: i64,
    enabled: bool,
    preview_limit: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ShelfConfigurationResponse {
    items: Vec<ShelfConfigurationItemResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ShelfConfigurationItemResponse {
    shelf_type: &'static str,
    saved_view_id: Option<i64>,
    position: u32,
    enabled: bool,
    preview_limit: u32,
}

impl From<ShelfConfigurationItem> for ShelfConfigurationItemResponse {
    fn from(item: ShelfConfigurationItem) -> Self {
        Self {
            shelf_type: item.shelf_type.as_str(),
            saved_view_id: item.saved_view_id,
            position: item.position,
            enabled: item.enabled,
            preview_limit: item.preview_limit,
        }
    }
}

pub(crate) async fn get_shelf_configuration<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<ShelfConfigurationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let items = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .shelf_configuration()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(response(items)))
}

pub(crate) async fn replace_shelf_configuration<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<ShelfConfigurationUpdateRequest>, JsonRejection>,
) -> Result<Json<ShelfConfigurationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) = payload.map_err(|_| {
        ApiError::bad_request(
            "invalid_shelf_configuration",
            "Shelf configuration JSON 無效",
        )
    })?;
    let items = payload
        .items
        .into_iter()
        .map(parse_item)
        .collect::<Result<Vec<_>, _>>()?;
    let items = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .replace_shelf_configuration(&items)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(response(items)))
}

pub(crate) async fn reset_shelf_configuration<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<ShelfConfigurationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let items = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        application
            .reset_shelf_configuration()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(response(items)))
}

fn response(items: Vec<ShelfConfigurationItem>) -> ShelfConfigurationResponse {
    ShelfConfigurationResponse {
        items: items.into_iter().map(Into::into).collect(),
    }
}

fn parse_item(request: ShelfConfigurationItemRequest) -> Result<ShelfConfigurationItem, ApiError> {
    let shelf_type = match request.shelf_type.as_str() {
        "recent" => ShelfType::Recent,
        "featured" => ShelfType::Featured,
        "event" => ShelfType::Event,
        "saved_view" => ShelfType::SavedView,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_shelf_configuration",
                "shelf_type 必須是 recent、featured、event 或 saved_view",
            ));
        }
    };
    let position = u32::try_from(request.position).map_err(|_| {
        ApiError::bad_request("invalid_shelf_configuration", "position 必須是非負整數")
    })?;
    let preview_limit = u32::try_from(request.preview_limit).map_err(|_| {
        ApiError::bad_request("invalid_shelf_configuration", "preview_limit 必須是正整數")
    })?;
    Ok(ShelfConfigurationItem {
        shelf_type,
        saved_view_id: request.saved_view_id,
        position,
        enabled: request.enabled,
        preview_limit,
    })
}
