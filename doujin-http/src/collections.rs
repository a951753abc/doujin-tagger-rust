//! Collection listing, lookup, launch and review queue endpoints.

use std::sync::TryLockError;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use doujin_app::{ApplicationError, ApplicationReadTargets};
use doujin_files::{LaunchAction, LaunchReceipt, RecycleBin};
use doujin_storage::StorageError;
use doujin_storage::collections::{
    CollectionPage, CollectionQueryLocation, CollectionRootSnapshot, CollectionSnapshot,
    ReviewQueuePage,
};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::metadata::MetadataHistoryResponse;
use crate::params::{
    parse_collection_id, parse_collection_query, parse_review_queue_query, source_name,
};
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Serialize)]
pub(crate) struct LaunchResponse {
    collection_id: i64,
    action: &'static str,
    launched: bool,
    entry_path: Option<String>,
}

impl From<LaunchReceipt> for LaunchResponse {
    fn from(receipt: LaunchReceipt) -> Self {
        Self {
            collection_id: receipt.collection_id,
            action: match receipt.action {
                LaunchAction::SystemDefault => "system_default",
                LaunchAction::ConfiguredReader => "configured_reader",
            },
            launched: true,
            entry_path: receipt.entry_path,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LaunchRequest {
    #[serde(default)]
    entry_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReadTargetsResponse {
    collection_id: i64,
    media_kind: &'static str,
    direct_image_count: usize,
    targets: Vec<ReadTargetResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReadTargetResponse {
    entry_path: String,
    image_count: usize,
}

pub(crate) async fn open_collection<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<LaunchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    launch_collection(
        state,
        collection_id,
        LaunchAction::SystemDefault,
        &headers,
        &body,
    )
    .await
}

pub(crate) async fn read_collection<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<LaunchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    launch_collection(
        state,
        collection_id,
        LaunchAction::ConfiguredReader,
        &headers,
        &body,
    )
    .await
}

/// 兩個啟動 endpoint 在子資料夾參數之前就存在，既有呼叫端不帶 request body，
/// 因此 body 自行解析：空 body 代表開啟收藏本身（不看 `Content-Type`），
/// 有內容時必須宣告 JSON media type 且是有效的 JSON。
fn parse_launch_entry(headers: &HeaderMap, body: &[u8]) -> Result<Option<String>, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    if !is_json_media_type(headers) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "request body 的 Content-Type 必須是 application/json",
        ));
    }
    let request: LaunchRequest = serde_json::from_slice(body)
        .map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    Ok(request.entry_path)
}

/// `application/json` 與 `application/<subtype>+json`，允許 `; charset=...` 之類的參數。
fn is_json_media_type(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .and_then(|essence| essence.split_once('/'))
        .is_some_and(|(kind, subtype)| {
            kind.eq_ignore_ascii_case("application")
                && (subtype.eq_ignore_ascii_case("json")
                    || subtype.to_ascii_lowercase().ends_with("+json"))
        })
}

pub(crate) async fn launch_collection<R>(
    state: HttpState<R>,
    collection_id: String,
    action: LaunchAction,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Json<LaunchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let entry = parse_launch_entry(headers, body)?;
    let receipt = tokio::task::spawn_blocking(move || {
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
        let entry = entry.as_deref();
        match action {
            LaunchAction::SystemDefault => application.open_collection(collection_id, entry),
            LaunchAction::ConfiguredReader => application.read_collection(collection_id, entry),
        }
        .map_err(ApiError::from_launch)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(receipt.into()))
}

pub(crate) async fn get_read_targets<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
) -> Result<Json<ReadTargetsResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let read_targets = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        match application.read_targets(collection_id) {
            Ok(read_targets) => Ok(read_targets),
            Err(ApplicationError::Storage(StorageError::CollectionNotFound(_))) => {
                match application.merged_into_collection(collection_id) {
                    Ok(Some(survivor_id)) => Err(ApiError::merged(survivor_id)),
                    Ok(None) => Err(ApiError::from_storage(StorageError::CollectionNotFound(
                        collection_id,
                    ))),
                    Err(error) => Err(ApiError::from_application(error)),
                }
            }
            Err(error) => Err(ApiError::from_application(error)),
        }
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(read_targets_response(collection_id, read_targets)))
}

fn read_targets_response(
    collection_id: i64,
    read_targets: ApplicationReadTargets,
) -> ReadTargetsResponse {
    ReadTargetsResponse {
        collection_id,
        media_kind: read_targets.media_kind.as_str(),
        direct_image_count: read_targets.direct_image_count,
        targets: read_targets
            .targets
            .into_iter()
            .map(|target| ReadTargetResponse {
                entry_path: target.entry_path,
                image_count: target.image_count,
            })
            .collect(),
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct CollectionPageResponse {
    items: Vec<CollectionResponse>,
    pagination: PaginationResponse,
}

#[derive(Debug, Serialize)]
pub(crate) struct PaginationResponse {
    page: u32,
    per_page: u32,
    total: i64,
    total_pages: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct CollectionResponse {
    id: i64,
    path: String,
    filename: String,
    media_kind: &'static str,
    root: Option<CollectionRootResponse>,
    title: Option<String>,
    event: Option<String>,
    circle: Option<String>,
    authors: Vec<String>,
    parody: Option<String>,
    parody_raw: Option<String>,
    classification_top: Option<String>,
    classification_subcategory: Option<String>,
    is_dl: Option<bool>,
    tags: Vec<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CollectionRootResponse {
    id: i64,
    source: &'static str,
    label: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CollectionQueryLocationResponse {
    status: &'static str,
    collection: CollectionResponse,
    position: Option<i64>,
    page: Option<u32>,
}

impl From<CollectionRootSnapshot> for CollectionRootResponse {
    fn from(root: CollectionRootSnapshot) -> Self {
        Self {
            id: root.id,
            source: source_name(root.source),
            label: root.label,
        }
    }
}

impl From<CollectionSnapshot> for CollectionResponse {
    fn from(collection: CollectionSnapshot) -> Self {
        Self {
            id: collection.id,
            path: collection.path.to_string_lossy().into_owned(),
            filename: collection.filename,
            media_kind: collection.media_kind.as_str(),
            root: collection.root.map(Into::into),
            title: collection.title,
            event: collection.event,
            circle: collection.circle,
            authors: collection.authors,
            parody: collection.parody,
            parody_raw: collection.parody_raw,
            classification_top: collection.classification_top,
            classification_subcategory: collection.classification_subcategory,
            is_dl: collection.is_dl,
            tags: collection.tags,
            created_at: collection.created_at,
            updated_at: collection.updated_at,
        }
    }
}

impl From<CollectionQueryLocation> for CollectionQueryLocationResponse {
    fn from(location: CollectionQueryLocation) -> Self {
        Self {
            status: if location.position.is_some() {
                "in_query"
            } else {
                "not_in_query"
            },
            collection: location.collection.into(),
            position: location.position,
            page: location.page,
        }
    }
}

impl From<CollectionPage> for CollectionPageResponse {
    fn from(page: CollectionPage) -> Self {
        let total_pages = if page.total == 0 {
            0
        } else {
            (page.total + i64::from(page.per_page) - 1) / i64::from(page.per_page)
        };
        Self {
            items: page.items.into_iter().map(Into::into).collect(),
            pagination: PaginationResponse {
                page: page.page,
                per_page: page.per_page,
                total: page.total,
                total_pages,
            },
        }
    }
}

pub(crate) async fn list_collections<R>(
    State(state): State<HttpState<R>>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<CollectionPageResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let query = parse_collection_query(raw_query.as_deref())?;
    let page = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .collections(&query)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(page.into()))
}

pub(crate) async fn list_review_queue<R>(
    State(state): State<HttpState<R>>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ReviewQueueResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let query = parse_review_queue_query(raw_query.as_deref())?;
    let page = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .review_queue(&query)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(page.try_into()?))
}

pub(crate) async fn locate_collection<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<CollectionQueryLocationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let query = parse_collection_query(raw_query.as_deref())?;
    let location = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .locate_collection(collection_id, &query)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(location.into()))
}

pub(crate) async fn get_collection<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
) -> Result<Json<CollectionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let collection = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        match application.collection(collection_id) {
            Ok(collection) => Ok(collection),
            Err(ApplicationError::Storage(StorageError::CollectionNotFound(_))) => {
                match application.merged_into_collection(collection_id) {
                    Ok(Some(survivor_id)) => Err(ApiError::merged(survivor_id)),
                    Ok(None) => Err(ApiError::from_storage(StorageError::CollectionNotFound(
                        collection_id,
                    ))),
                    Err(error) => Err(ApiError::from_application(error)),
                }
            }
            Err(error) => Err(ApiError::from_application(error)),
        }
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(collection.into()))
}

#[derive(Debug, Serialize)]
pub(crate) struct ReviewQueueResponse {
    items: Vec<ReviewQueueItemResponse>,
    pagination: PaginationResponse,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReviewQueueItemResponse {
    collection: CollectionResponse,
    metadata: MetadataHistoryResponse,
}

impl TryFrom<ReviewQueuePage> for ReviewQueueResponse {
    type Error = ApiError;

    fn try_from(page: ReviewQueuePage) -> Result<Self, Self::Error> {
        let total_pages = if page.total == 0 {
            0
        } else {
            (page.total + i64::from(page.per_page) - 1) / i64::from(page.per_page)
        };
        Ok(Self {
            items: page
                .items
                .into_iter()
                .map(|item| {
                    Ok(ReviewQueueItemResponse {
                        collection: item.collection.into(),
                        metadata: item.metadata.try_into()?,
                    })
                })
                .collect::<Result<_, ApiError>>()?,
            pagination: PaginationResponse {
                page: page.page,
                per_page: page.per_page,
                total: page.total,
                total_pages,
            },
        })
    }
}
