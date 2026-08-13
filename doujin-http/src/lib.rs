//! Thin localhost-only HTTP adapter for the Rust application service.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::uri::Authority;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use doujin_app::external_search::{ExternalSearchBatchFieldNeed, ExternalSearchBatchPreflight};
use doujin_app::rename::{RenameExpectedItem, RenamePreflight};
use doujin_app::{
    ApplicationBatchOutcome, ApplicationBatchReport, ApplicationError, ApplicationScanExpectation,
    ApplicationScanMode, ApplicationScanOptions, ApplicationScanPreflight, ApplicationScanReport,
    ApplicationService, ApplicationSettingsSnapshot, SavedViewWithCount,
};
use doujin_files::{
    BatchReport, DeleteRequest, ItemStatus, LaunchAction, LaunchError, LaunchReceipt, RecycleBin,
};
use doujin_parser::domain::{Authors, Classification, Parody};
use doujin_scanner::SourceKind;
use doujin_storage::StorageError;
use doujin_storage::collections::{
    CollectionPage, CollectionQuery, CollectionQueryLocation, CollectionRootSnapshot,
    CollectionSnapshot, CollectionSort, MissingMetadataField, ReviewQueueKind, ReviewQueuePage,
    ReviewQueueQuery, SortDirection,
};
use doujin_storage::consolidation::{
    ConsolidationChoice, ConsolidationConflict, ConsolidationPreflight, ConsolidationResolution,
    ConsolidationSnapshot, ManualSelectionEvidence,
};
use doujin_storage::duplicates::{
    DuplicateCandidatePair, DuplicateCollectionEvidence, DuplicateScanItemSnapshot,
    DuplicateScanJobSnapshot,
};
use doujin_storage::external_search_batches::{
    ExternalSearchBatchItemSnapshot, ExternalSearchBatchSnapshot, ExternalSearchBatchStrategy,
};
use doujin_storage::jobs::{ExternalSearchEnqueueOutcome, ExternalSearchJobSnapshot};
use doujin_storage::lifecycle::{CandidateDecision, DeleteMode, TombstoneCandidateSnapshot};
use doujin_storage::metadata::{
    ExternalSearchResultHistory, MetadataAssertionDecision, MetadataAssertionHistory,
    MetadataField, MetadataFieldHistory, MetadataHistory, MetadataSelectionHistory, MetadataValue,
};
use doujin_storage::roots::LibraryRootSnapshot;
use doujin_storage::saved_views::{SavedViewLayout, SavedViewQuery};
use doujin_storage::scan::{ScanIssueSnapshot, ScanRunSnapshot};
use doujin_storage::statistics::{CollectionFacet, CollectionStatistics, NamedCount};
use doujin_storage::thumbnails::{
    DEFAULT_THUMBNAIL_PRIORITY, MAX_THUMBNAIL_PRIORITY, ThumbnailStateSnapshot, ThumbnailStatus,
};
use doujin_storage::vocabulary::{
    VocabularyCandidateGroup, VocabularyField, VocabularyMergePreflight, VocabularyMergeResult,
};
use doujin_storage::work_baskets::{WorkBasketItemSnapshot, WorkBasketSnapshot, WorkBasketSummary};
use doujin_thumbnails::transparent_placeholder_webp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpListener;

#[derive(Debug)]
pub enum ServerError {
    Io(std::io::Error),
    NonLoopbackAddress(SocketAddr),
}

impl fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "HTTP server I/O 錯誤：{error}"),
            Self::NonLoopbackAddress(address) => {
                write!(
                    formatter,
                    "HTTP server 只允許 localhost loopback：{address}"
                )
            }
        }
    }
}

impl Error for ServerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::NonLoopbackAddress(_) => None,
        }
    }
}

impl From<std::io::Error> for ServerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn validate_loopback_address(address: SocketAddr) -> Result<(), ServerError> {
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(ServerError::NonLoopbackAddress(address))
    }
}

pub async fn bind_loopback(address: SocketAddr) -> Result<TcpListener, ServerError> {
    validate_loopback_address(address)?;
    Ok(TcpListener::bind(address).await?)
}

pub async fn serve_with_shutdown<R, F>(
    listener: TcpListener,
    application: ApplicationService<R>,
    shutdown: F,
) -> Result<(), ServerError>
where
    R: RecycleBin + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    serve_shared_with_shutdown(listener, share_application(application), shutdown).await
}

pub type SharedApplication<R> = Arc<Mutex<ApplicationService<R>>>;

pub fn share_application<R>(application: ApplicationService<R>) -> SharedApplication<R> {
    Arc::new(Mutex::new(application))
}

pub async fn serve_shared_with_shutdown<R, F>(
    listener: TcpListener,
    application: SharedApplication<R>,
    shutdown: F,
) -> Result<(), ServerError>
where
    R: RecycleBin + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    validate_loopback_address(listener.local_addr()?)?;
    axum::serve(listener, build_router(application))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

fn build_router<R>(application: SharedApplication<R>) -> Router
where
    R: RecycleBin + Send + 'static,
{
    let state = HttpState {
        application,
        thumbnail_cache_jobs: Arc::new(Mutex::new(ThumbnailCacheJobs::default())),
    };
    Router::new()
        .route("/", get(frontend_index))
        .route("/assets/app.css", get(frontend_css))
        .route("/assets/app.js", get(frontend_javascript))
        .route("/api/health", get(health))
        .route(
            "/api/settings",
            get(get_settings::<R>).put(update_settings::<R>),
        )
        .route("/api/stats", get(get_statistics::<R>))
        .route("/api/facets", get(get_facets::<R>))
        .route("/api/duplicate-jobs", post(start_duplicate_scan::<R>))
        .route(
            "/api/duplicate-jobs/current",
            get(get_current_duplicate_scan::<R>),
        )
        .route("/api/duplicate-jobs/{job_id}", get(get_duplicate_scan::<R>))
        .route(
            "/api/duplicate-jobs/{job_id}/failures",
            get(get_duplicate_scan_failures::<R>),
        )
        .route(
            "/api/duplicate-jobs/{job_id}/retry-failures",
            post(retry_duplicate_scan_failures::<R>),
        )
        .route("/api/duplicates", get(list_duplicate_candidates::<R>))
        .route(
            "/api/duplicates/{left_collection_id}/{right_collection_id}/exclude",
            post(exclude_duplicate_pair::<R>),
        )
        .route(
            "/api/duplicates/{left_collection_id}/{right_collection_id}/confirm",
            post(confirm_duplicate_pair::<R>),
        )
        .route(
            "/api/vocabulary/candidates",
            get(list_vocabulary_candidates::<R>),
        )
        .route(
            "/api/vocabulary/preflight",
            post(preflight_vocabulary_merge::<R>),
        )
        .route("/api/vocabulary/merge", post(merge_vocabulary::<R>))
        .route("/api/vocabulary/reject", post(reject_vocabulary::<R>))
        .route(
            "/api/saved-views",
            get(list_saved_views::<R>).post(create_saved_view::<R>),
        )
        .route(
            "/api/saved-views/{saved_view_id}",
            get(get_saved_view::<R>)
                .put(update_saved_view::<R>)
                .delete(delete_saved_view::<R>),
        )
        .route("/api/work-baskets", get(list_work_baskets::<R>))
        .route("/api/work-baskets/{basket_id}", get(get_work_basket::<R>))
        .route(
            "/api/work-baskets/{basket_id}/collections",
            post(add_work_basket_collections::<R>).delete(clear_work_basket::<R>),
        )
        .route(
            "/api/work-baskets/{basket_id}/collections/{collection_id}",
            axum::routing::delete(remove_work_basket_collection::<R>),
        )
        .route("/api/collections", get(list_collections::<R>))
        .route("/api/review-queue", get(list_review_queue::<R>))
        .route(
            "/api/collections/{collection_id}/locate",
            get(locate_collection::<R>),
        )
        .route("/api/collections/{collection_id}", get(get_collection::<R>))
        .route(
            "/api/collections/{collection_id}/open",
            post(open_collection::<R>),
        )
        .route(
            "/api/collections/{collection_id}/read",
            post(read_collection::<R>),
        )
        .route(
            "/api/collections/{collection_id}/thumbnail",
            get(get_thumbnail::<R>),
        )
        .route(
            "/api/collections/{collection_id}/thumbnail/rebuild",
            post(rebuild_thumbnail::<R>),
        )
        .route(
            "/api/collections/{collection_id}/cover-candidates",
            get(get_cover_candidates::<R>),
        )
        .route(
            "/api/collections/{collection_id}/cover-candidates/preview",
            get(get_cover_candidate_preview::<R>),
        )
        .route(
            "/api/collections/{collection_id}/cover-selection",
            put(put_cover_selection::<R>).delete(delete_cover_selection::<R>),
        )
        .route("/api/thumbnails/rebuild", post(rebuild_all_thumbnails::<R>))
        .route(
            "/api/thumbnail-cache-jobs",
            post(start_thumbnail_cache_job::<R>),
        )
        .route(
            "/api/thumbnail-cache-jobs/preflight",
            post(preflight_thumbnail_cache_job::<R>),
        )
        .route(
            "/api/thumbnail-cache-jobs/current",
            get(get_current_thumbnail_cache_job::<R>),
        )
        .route(
            "/api/thumbnail-cache-jobs/current/failures",
            get(get_thumbnail_cache_failures::<R>),
        )
        .route(
            "/api/thumbnail-cache-jobs/current/retry-failures",
            post(retry_thumbnail_cache_failures::<R>),
        )
        .route(
            "/api/collections/{collection_id}/metadata",
            get(get_metadata_history::<R>),
        )
        .route(
            "/api/collections/{collection_id}/metadata/{field}",
            put(set_manual_metadata::<R>).delete(clear_manual_metadata::<R>),
        )
        .route(
            "/api/collections/{collection_id}/metadata/{field}/assertions/{assertion_id}",
            patch(decide_metadata_assertion::<R>),
        )
        .route(
            "/api/collections/{collection_id}/tags",
            post(add_collection_tag::<R>).delete(remove_collection_tag::<R>),
        )
        .route("/api/batch/tags", post(batch_add_collection_tag::<R>))
        .route(
            "/api/batch/metadata/{field}",
            put(batch_set_manual_metadata::<R>),
        )
        .route(
            "/api/collections/{collection_id}/external-search-jobs",
            post(enqueue_external_search::<R>),
        )
        .route(
            "/api/external-search-jobs/{job_id}",
            get(get_external_search_job::<R>),
        )
        .route(
            "/api/external-search-batches/preflight",
            post(preflight_external_search_batch::<R>),
        )
        .route(
            "/api/external-search-batches",
            post(create_external_search_batch::<R>),
        )
        .route(
            "/api/external-search-batches/{batch_id}",
            get(get_external_search_batch::<R>),
        )
        .route(
            "/api/external-search-batches/{batch_id}/retry",
            post(retry_external_search_batch::<R>),
        )
        .route(
            "/api/tombstone-candidates",
            get(list_tombstone_candidates::<R>),
        )
        .route(
            "/api/tombstone-candidates/{tombstone_id}/{candidate_id}",
            patch(decide_tombstone_candidate::<R>),
        )
        .route(
            "/api/tombstone-candidates/{tombstone_id}/{candidate_id}/preflight",
            get(consolidation_preflight::<R>),
        )
        .route(
            "/api/tombstone-candidates/{tombstone_id}/{candidate_id}/consolidate",
            post(consolidate_tombstone_candidate::<R>),
        )
        .route(
            "/api/library-roots",
            get(list_library_roots::<R>).post(register_library_root::<R>),
        )
        .route(
            "/api/library-roots/{root_id}",
            patch(update_library_root::<R>).delete(deactivate_library_root::<R>),
        )
        .route(
            "/api/library-roots/{root_id}/activate",
            post(reactivate_library_root::<R>),
        )
        .route("/api/file-actions/move", post(move_collections::<R>))
        .route(
            "/api/file-actions/rename/preflight",
            post(preflight_rename_collections::<R>),
        )
        .route("/api/file-actions/rename", post(rename_collections::<R>))
        .route("/api/file-actions/delete", post(delete_collections::<R>))
        .route("/api/scans", post(start_scan::<R>))
        .route("/api/scans/preflight", post(preflight_scan::<R>))
        .route("/api/scans/latest", get(get_latest_scan::<R>))
        .route("/api/scans/{scan_run_id}", get(get_scan::<R>))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn(request_guard))
        .with_state(state)
}

struct HttpState<R> {
    application: Arc<Mutex<ApplicationService<R>>>,
    thumbnail_cache_jobs: Arc<Mutex<ThumbnailCacheJobs>>,
}

impl<R> Clone for HttpState<R> {
    fn clone(&self) -> Self {
        Self {
            application: Arc::clone(&self.application),
            thumbnail_cache_jobs: Arc::clone(&self.thumbnail_cache_jobs),
        }
    }
}

#[derive(Default)]
struct ThumbnailCacheJobs {
    next_id: u64,
    current: Option<ThumbnailCacheJob>,
}

struct ThumbnailCacheJob {
    id: u64,
    root_ids: Vec<i64>,
    collection_ids: Vec<i64>,
    failed_collection_ids: Vec<i64>,
    initial_completed: usize,
    started_at: Instant,
}

const INTERACTIVE_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const INTERACTIVE_LOCK_RETRY: Duration = Duration::from_millis(10);

fn lock_interactive_application<R>(
    application: &Mutex<ApplicationService<R>>,
) -> Result<MutexGuard<'_, ApplicationService<R>>, ApiError> {
    let deadline = Instant::now() + INTERACTIVE_LOCK_TIMEOUT;
    loop {
        match application.try_lock() {
            Ok(application) => return Ok(application),
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(INTERACTIVE_LOCK_RETRY);
            }
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
        }
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    api_version: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance_id: Option<String>,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "doujin-http",
        api_version: 1,
        instance_id: std::env::var("DOUJIN_INSTANCE_ID")
            .ok()
            .filter(|value| !value.is_empty()),
    })
}

const FRONTEND_INDEX: &str = include_str!("../static/index.html");
const FRONTEND_CSS: &str = include_str!("../static/app.css");
const FRONTEND_JAVASCRIPT: &str = include_str!("../static/app.js");

async fn frontend_index() -> Response {
    frontend_response(FRONTEND_INDEX, "text/html; charset=utf-8", "no-store", true)
}

async fn frontend_css() -> Response {
    frontend_response(FRONTEND_CSS, "text/css; charset=utf-8", "no-cache", false)
}

async fn frontend_javascript() -> Response {
    frontend_response(
        FRONTEND_JAVASCRIPT,
        "text/javascript; charset=utf-8",
        "no-cache",
        false,
    )
}

fn frontend_response(
    body: &'static str,
    content_type: &'static str,
    cache_control: &'static str,
    is_document: bool,
) -> Response {
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert("content-type", HeaderValue::from_static(content_type));
    headers.insert("cache-control", HeaderValue::from_static(cache_control));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if is_document {
        headers.insert(
            "content-security-policy",
            HeaderValue::from_static(
                "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
            ),
        );
    }
    response
}

#[derive(Debug, Serialize)]
struct SettingsResponse {
    viewer_path: String,
    thumb_size: String,
    thumb_quality: u8,
    saved_viewer_path: String,
    saved_thumb_size: String,
    saved_thumb_quality: u8,
    overrides: SettingsOverridesResponse,
    environment_overrides: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thumbnails_requeued: Option<usize>,
}

#[derive(Debug, Serialize)]
struct SettingsOverridesResponse {
    viewer_path: Option<&'static str>,
    thumb_size: Option<&'static str>,
    thumb_quality: Option<&'static str>,
}

impl SettingsResponse {
    fn from_snapshot(
        settings: ApplicationSettingsSnapshot,
        thumbnails_requeued: Option<usize>,
    ) -> Self {
        let mut environment_overrides = Vec::new();
        if settings.reader_overridden_by_environment {
            environment_overrides.push("DOUJIN_READER_PATH");
        }
        if settings.thumbnail_size_overridden_by_environment {
            environment_overrides.push("DOUJIN_THUMB_SIZE");
        }
        if settings.thumbnail_quality_overridden_by_environment {
            environment_overrides.push("DOUJIN_THUMB_QUALITY");
        }
        Self {
            viewer_path: settings
                .reader_path
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            thumb_size: format!("{}x{}", settings.thumbnail_width, settings.thumbnail_height),
            thumb_quality: settings.thumbnail_quality,
            saved_viewer_path: settings
                .saved_reader_path
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            saved_thumb_size: format!(
                "{}x{}",
                settings.saved_thumbnail_width, settings.saved_thumbnail_height
            ),
            saved_thumb_quality: settings.saved_thumbnail_quality,
            overrides: SettingsOverridesResponse {
                viewer_path: settings
                    .reader_overridden_by_environment
                    .then_some("DOUJIN_READER_PATH"),
                thumb_size: settings
                    .thumbnail_size_overridden_by_environment
                    .then_some("DOUJIN_THUMB_SIZE"),
                thumb_quality: settings
                    .thumbnail_quality_overridden_by_environment
                    .then_some("DOUJIN_THUMB_QUALITY"),
            },
            environment_overrides,
            thumbnails_requeued,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsUpdateRequest {
    viewer_path: String,
    thumb_size: String,
    thumb_quality: i64,
}

async fn get_settings<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<SettingsResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let settings = tokio::task::spawn_blocking(move || {
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
        application
            .application_settings()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(SettingsResponse::from_snapshot(settings, None)))
}

async fn update_settings<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<SettingsUpdateRequest>, JsonRejection>,
) -> Result<Json<SettingsResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let (width, height) = parse_thumbnail_size(&payload.thumb_size)?;
    let quality = u8::try_from(payload.thumb_quality)
        .ok()
        .filter(|quality| (1..=100).contains(quality))
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_thumbnail_quality",
                "thumb_quality 必須是 1 到 100 的整數",
            )
        })?;
    let reader_path = match payload.viewer_path.trim() {
        "" => None,
        value => Some(PathBuf::from(value)),
    };
    if let Some(reader_path) = reader_path.as_deref()
        && (!reader_path.is_absolute() || !reader_path.is_file())
    {
        let message = format!(
            "閱讀器不存在或不是一般檔案：{}。若要使用系統預設程式，請將閱讀器欄位留空",
            reader_path.display()
        );
        return Err(ApiError::bad_request("invalid_reader_path", &message));
    }
    let outcome = tokio::task::spawn_blocking(move || {
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
            .save_application_settings(reader_path, width, height, quality)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(SettingsResponse::from_snapshot(
        outcome.settings,
        Some(outcome.thumbnails_requeued),
    )))
}

fn parse_thumbnail_size(value: &str) -> Result<(u32, u32), ApiError> {
    let value = value.trim();
    let (width, height) = value.split_once('x').ok_or_else(|| {
        ApiError::bad_request(
            "invalid_thumbnail_size",
            "thumb_size 必須使用 WIDTHxHEIGHT 格式",
        )
    })?;
    let width = width.parse::<u32>().ok();
    let height = height.parse::<u32>().ok();
    match (width, height) {
        (Some(width), Some(height))
            if (1..=4096).contains(&width) && (1..=4096).contains(&height) =>
        {
            Ok((width, height))
        }
        _ => Err(ApiError::bad_request(
            "invalid_thumbnail_size",
            "thumb_size 的寬高必須是 1 到 4096 的整數",
        )),
    }
}

#[derive(Debug, Serialize)]
struct NamedCountResponse {
    name: String,
    count: i64,
}

impl From<NamedCount> for NamedCountResponse {
    fn from(value: NamedCount) -> Self {
        Self {
            name: value.name,
            count: value.count,
        }
    }
}

#[derive(Debug, Serialize)]
struct StatisticsResponse {
    total: i64,
    tagged: i64,
    missing_metadata: i64,
    categories: Vec<NamedCountResponse>,
    top_parody: Vec<NamedCountResponse>,
    top_author: Vec<NamedCountResponse>,
    top_circle: Vec<NamedCountResponse>,
    top_event: Vec<NamedCountResponse>,
    top_tags: Vec<NamedCountResponse>,
}

impl From<CollectionStatistics> for StatisticsResponse {
    fn from(value: CollectionStatistics) -> Self {
        Self {
            total: value.total,
            tagged: value.tagged,
            missing_metadata: value.missing_metadata,
            categories: value.classifications.into_iter().map(Into::into).collect(),
            top_parody: value.top_parodies.into_iter().map(Into::into).collect(),
            top_author: value.top_authors.into_iter().map(Into::into).collect(),
            top_circle: value.top_circles.into_iter().map(Into::into).collect(),
            top_event: value.top_events.into_iter().map(Into::into).collect(),
            top_tags: value.top_tags.into_iter().map(Into::into).collect(),
        }
    }
}

async fn get_statistics<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<StatisticsResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let statistics = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .collection_statistics()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(statistics.into()))
}

#[derive(Debug, Serialize)]
struct FacetResponse {
    items: Vec<NamedCountResponse>,
}

async fn get_facets<R>(
    State(state): State<HttpState<R>>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<FacetResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let (facet, search, limit) = parse_facet_query(raw_query.as_deref())?;
    let items = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .collection_facets(facet, &search, limit)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(FacetResponse {
        items: items.into_iter().map(Into::into).collect(),
    }))
}

#[derive(Debug, Serialize)]
struct DuplicateScanJobResponse {
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
struct DuplicateCandidatesResponse {
    items: Vec<DuplicateCandidateResponse>,
    total: usize,
}

#[derive(Debug, Serialize)]
struct DuplicateCandidateResponse {
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
struct DuplicateEvidenceResponse {
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

async fn start_duplicate_scan<R>(
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

async fn get_duplicate_scan<R>(
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
struct DuplicateScanJobEnvelope {
    job: Option<DuplicateScanJobResponse>,
}

async fn get_current_duplicate_scan<R>(
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
struct DuplicateScanFailuresResponse {
    items: Vec<DuplicateScanFailureResponse>,
}

#[derive(Debug, Serialize)]
struct DuplicateScanFailureResponse {
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

async fn get_duplicate_scan_failures<R>(
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

async fn retry_duplicate_scan_failures<R>(
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

async fn list_duplicate_candidates<R>(
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
struct DuplicateDecisionRequest {
    left_fingerprint_identity: String,
    right_fingerprint_identity: String,
}

#[derive(Debug, Serialize)]
struct DuplicateDecisionResponse {
    status: &'static str,
}

async fn exclude_duplicate_pair<R>(
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

async fn confirm_duplicate_pair<R>(
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

async fn decide_duplicate_pair<R>(
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

fn parse_positive_id(value: &str, label: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_id", &format!("{label} 必須是正整數")))
}

#[derive(Debug, Serialize)]
struct VocabularyCandidatesResponse {
    groups: Vec<VocabularyCandidateGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VocabularyMergeRequest {
    field: String,
    canonical: String,
    variants: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VocabularyRejectRequest {
    field: String,
    values: Vec<String>,
    reason: String,
    #[serde(default)]
    removed: bool,
}

#[derive(Debug, Serialize)]
struct VocabularyRejectResponse {
    exclusions_recorded: usize,
}

async fn list_vocabulary_candidates<R>(
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

async fn preflight_vocabulary_merge<R>(
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

async fn merge_vocabulary<R>(
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

async fn reject_vocabulary<R>(
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

#[derive(Debug, Serialize)]
struct LaunchResponse {
    collection_id: i64,
    action: &'static str,
    launched: bool,
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
        }
    }
}

async fn open_collection<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
) -> Result<Json<LaunchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    launch_collection(state, collection_id, LaunchAction::SystemDefault).await
}

async fn read_collection<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
) -> Result<Json<LaunchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    launch_collection(state, collection_id, LaunchAction::ConfiguredReader).await
}

async fn get_thumbnail<R>(
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
struct ThumbnailStateResponse {
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

fn parse_thumbnail_priority(raw_query: Option<&str>) -> Result<i64, ApiError> {
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

async fn rebuild_thumbnail<R>(
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

async fn get_cover_candidates<R>(
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

fn parse_cover_candidate_limit(raw_query: Option<&str>) -> Result<usize, ApiError> {
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

async fn get_cover_candidate_preview<R>(
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

fn parse_cover_entry_query(raw_query: Option<&str>) -> Result<String, ApiError> {
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
struct CoverSelectionPayload {
    entry_path: String,
    source_fingerprint: String,
}

async fn put_cover_selection<R>(
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

async fn delete_cover_selection<R>(
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

#[derive(Debug, Serialize)]
struct ThumbnailRebuildAllResponse {
    rebuilt: usize,
}

async fn rebuild_all_thumbnails<R>(
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
struct ThumbnailCacheJobRequest {
    root_ids: Vec<i64>,
}

#[derive(Debug, Serialize)]
struct ThumbnailCachePreflightResponse {
    root_ids: Vec<i64>,
    root_count: usize,
    collection_count: usize,
    ready: usize,
    requires_build: usize,
    known_failures: usize,
    cancellation_supported: bool,
}

#[derive(Debug, Serialize)]
struct ThumbnailCacheJobEnvelope {
    job: Option<ThumbnailCacheJobResponse>,
}

#[derive(Debug, Serialize)]
struct ThumbnailCacheJobResponse {
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
struct ThumbnailCacheFailuresResponse {
    job_id: Option<u64>,
    items: Vec<CollectionResponse>,
    missing_collection_ids: Vec<i64>,
}

async fn preflight_thumbnail_cache_job<R>(
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

async fn start_thumbnail_cache_job<R>(
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

async fn get_current_thumbnail_cache_job<R>(
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

async fn get_thumbnail_cache_failures<R>(
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

async fn retry_thumbnail_cache_failures<R>(
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

fn thumbnail_cache_failed_ids<R: RecycleBin>(
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

fn thumbnail_cache_job_response<R: RecycleBin>(
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

async fn launch_collection<R>(
    state: HttpState<R>,
    collection_id: String,
    action: LaunchAction,
) -> Result<Json<LaunchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
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
        match action {
            LaunchAction::SystemDefault => application.open_collection(collection_id),
            LaunchAction::ConfiguredReader => application.read_collection(collection_id),
        }
        .map_err(ApiError::from_launch)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(receipt.into()))
}

#[derive(Debug, Serialize)]
struct CollectionPageResponse {
    items: Vec<CollectionResponse>,
    pagination: PaginationResponse,
}

#[derive(Debug, Serialize)]
struct PaginationResponse {
    page: u32,
    per_page: u32,
    total: i64,
    total_pages: i64,
}

#[derive(Debug, Serialize)]
struct CollectionResponse {
    id: i64,
    path: String,
    filename: String,
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
struct CollectionRootResponse {
    id: i64,
    source: &'static str,
    label: String,
}

#[derive(Debug, Serialize)]
struct CollectionQueryLocationResponse {
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

#[derive(Debug, Serialize)]
struct WorkBasketsResponse {
    baskets: Vec<WorkBasketSummaryResponse>,
}

#[derive(Debug, Serialize)]
struct WorkBasketSummaryResponse {
    id: i64,
    name: String,
    count: i64,
}

#[derive(Debug, Serialize)]
struct WorkBasketResponse {
    id: i64,
    name: String,
    count: usize,
    items: Vec<WorkBasketItemResponse>,
}

#[derive(Debug, Serialize)]
struct WorkBasketItemResponse {
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedViewMutationRequest {
    name: String,
    query: SavedViewQueryRequest,
    #[serde(default = "default_true")]
    pinned: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedViewQueryRequest {
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
struct SavedViewsResponse {
    items: Vec<SavedViewResponse>,
}

#[derive(Debug, Serialize)]
struct SavedViewResponse {
    id: i64,
    name: String,
    query: SavedViewQueryResponse,
    pinned: bool,
    result_count: i64,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
struct SavedViewQueryResponse {
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

async fn list_saved_views<R>(
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

async fn get_saved_view<R>(
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

async fn create_saved_view<R>(
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

async fn update_saved_view<R>(
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

async fn delete_saved_view<R>(
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

fn saved_view_query(request: SavedViewQueryRequest) -> Result<SavedViewQuery, ApiError> {
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

fn append_optional_parameter(
    serializer: &mut form_urlencoded::Serializer<'_, String>,
    name: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        serializer.append_pair(name, value);
    }
}

fn saved_view_query_response(query: SavedViewQuery) -> SavedViewQueryResponse {
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

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct WorkBasketCollectionsRequest {
    collection_ids: Vec<i64>,
}

async fn list_work_baskets<R>(
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

async fn get_work_basket<R>(
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

async fn add_work_basket_collections<R>(
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

async fn remove_work_basket_collection<R>(
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

async fn clear_work_basket<R>(
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

async fn list_collections<R>(
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

async fn list_review_queue<R>(
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

async fn locate_collection<R>(
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

async fn get_collection<R>(
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
struct MetadataHistoryResponse {
    collection_id: i64,
    fields: Vec<MetadataFieldHistoryResponse>,
}

#[derive(Debug, Serialize)]
struct MetadataFieldHistoryResponse {
    field: &'static str,
    selection: Option<MetadataSelectionResponse>,
    assertions: Vec<MetadataAssertionResponse>,
    external_search_results: Vec<ExternalSearchResultResponse>,
}

#[derive(Debug, Serialize)]
struct MetadataSelectionResponse {
    assertion_id: i64,
    selected_by: &'static str,
    selected_at: String,
}

#[derive(Debug, Serialize)]
struct MetadataAssertionResponse {
    id: i64,
    value: Value,
    source: &'static str,
    parser_run_id: Option<i64>,
    source_reference: Option<String>,
    confidence_total: Option<f64>,
    confidence: Option<Value>,
    status: &'static str,
    reason: Option<String>,
    created_at: String,
    selected: bool,
}

#[derive(Debug, Serialize)]
struct ExternalSearchResultResponse {
    id: i64,
    value: Value,
    source_reference: String,
    confidence_total: f64,
    confidence: Value,
    disposition: &'static str,
    assertion_id: Option<i64>,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct ReviewQueueResponse {
    items: Vec<ReviewQueueItemResponse>,
    pagination: PaginationResponse,
}

#[derive(Debug, Serialize)]
struct ReviewQueueItemResponse {
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

impl TryFrom<MetadataHistory> for MetadataHistoryResponse {
    type Error = ApiError;

    fn try_from(history: MetadataHistory) -> Result<Self, Self::Error> {
        Ok(Self {
            collection_id: history.collection_id,
            fields: history
                .fields
                .into_iter()
                .map(metadata_field_history_response)
                .collect::<Result<_, _>>()?,
        })
    }
}

fn metadata_field_history_response(
    history: MetadataFieldHistory,
) -> Result<MetadataFieldHistoryResponse, ApiError> {
    let selected_assertion_id = history
        .selection
        .as_ref()
        .map(|selection| selection.assertion_id);
    Ok(MetadataFieldHistoryResponse {
        field: history.field.as_str(),
        selection: history.selection.map(metadata_selection_response),
        assertions: history
            .assertions
            .into_iter()
            .map(|assertion| metadata_assertion_response(assertion, selected_assertion_id))
            .collect::<Result<_, _>>()?,
        external_search_results: history
            .external_search_results
            .into_iter()
            .map(external_search_result_response)
            .collect::<Result<_, _>>()?,
    })
}

fn metadata_selection_response(selection: MetadataSelectionHistory) -> MetadataSelectionResponse {
    MetadataSelectionResponse {
        assertion_id: selection.assertion_id,
        selected_by: selection.selected_by.as_str(),
        selected_at: selection.selected_at,
    }
}

fn metadata_assertion_response(
    assertion: MetadataAssertionHistory,
    selected_assertion_id: Option<i64>,
) -> Result<MetadataAssertionResponse, ApiError> {
    let value = serde_json::from_str(&assertion.value_json).map_err(|_| ApiError::internal())?;
    let confidence = assertion
        .confidence_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| ApiError::internal())?;
    Ok(MetadataAssertionResponse {
        id: assertion.id,
        value,
        source: assertion.source.as_str(),
        parser_run_id: assertion.parser_run_id,
        source_reference: assertion.source_reference,
        confidence_total: assertion.confidence_total,
        confidence,
        status: assertion.status.as_str(),
        reason: assertion.reason,
        created_at: assertion.created_at,
        selected: selected_assertion_id == Some(assertion.id),
    })
}

fn external_search_result_response(
    result: ExternalSearchResultHistory,
) -> Result<ExternalSearchResultResponse, ApiError> {
    Ok(ExternalSearchResultResponse {
        id: result.id,
        value: serde_json::from_str(&result.value_json).map_err(|_| ApiError::internal())?,
        source_reference: result.source_reference,
        confidence_total: result.confidence_total,
        confidence: serde_json::from_str(&result.confidence_json)
            .map_err(|_| ApiError::internal())?,
        disposition: result.disposition.as_str(),
        assertion_id: result.assertion_id,
        created_at: result.created_at,
    })
}

async fn get_metadata_history<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
) -> Result<Json<MetadataHistoryResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let history = tokio::task::spawn_blocking(move || {
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
        application
            .metadata_history(collection_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(history.try_into()?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualMetadataRequest {
    value: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataAssertionDecisionRequest {
    decision: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TagRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchTagRequest {
    collection_ids: Vec<i64>,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchMetadataRequest {
    collection_ids: Vec<i64>,
    value: Value,
}

#[derive(Debug, Serialize)]
struct BatchMutationResponse {
    summary: BatchMutationSummaryResponse,
    items: Vec<BatchMutationItemResponse>,
}

#[derive(Debug, Serialize)]
struct BatchMutationSummaryResponse {
    total: usize,
    completed: usize,
    succeeded: usize,
    unchanged: usize,
    failed: usize,
}

#[derive(Debug, Serialize)]
struct BatchMutationItemResponse {
    collection_id: i64,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    collection: Option<CollectionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<BatchMutationErrorResponse>,
}

#[derive(Debug, Serialize)]
struct BatchMutationErrorResponse {
    code: &'static str,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalSearchJobRequest {
    fields: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalSearchBatchRequest {
    collection_ids: Vec<i64>,
    fields: Vec<String>,
    strategy: String,
}

#[derive(Debug, Serialize)]
struct ExternalSearchBatchFieldNeedResponse {
    field: &'static str,
    count: usize,
}

#[derive(Debug, Serialize)]
struct ExternalSearchBatchPreflightItemResponse {
    collection_id: i64,
    fields: Vec<&'static str>,
    outcome: &'static str,
    job_id: Option<i64>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExternalSearchBatchPreflightResponse {
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
struct ExternalSearchBatchSummaryResponse {
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
struct ExternalSearchBatchItemResponse {
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
struct ExternalSearchBatchResponse {
    id: i64,
    strategy: &'static str,
    fields: Vec<&'static str>,
    created_at: String,
    summary: ExternalSearchBatchSummaryResponse,
    items: Vec<ExternalSearchBatchItemResponse>,
}

#[derive(Debug, Serialize)]
struct ExternalSearchEnqueueResponse {
    created: bool,
    job: ExternalSearchJobResponse,
}

#[derive(Debug, Serialize)]
struct ExternalSearchJobResponse {
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TombstoneCandidateDecisionRequest {
    decision: String,
}

#[derive(Debug, Serialize)]
struct TombstoneCandidatesResponse {
    items: Vec<TombstoneCandidateResponse>,
}

#[derive(Debug, Serialize)]
struct TombstoneCandidateResponse {
    tombstone_collection_id: i64,
    candidate_collection_id: i64,
    tombstone_path: String,
    candidate_path: Option<String>,
    reason: String,
    decision: &'static str,
    discovered_at: String,
    decided_at: Option<String>,
}

impl From<TombstoneCandidateSnapshot> for TombstoneCandidateResponse {
    fn from(candidate: TombstoneCandidateSnapshot) -> Self {
        Self {
            tombstone_collection_id: candidate.tombstone_collection_id,
            candidate_collection_id: candidate.candidate_collection_id,
            tombstone_path: candidate.tombstone_path.to_string_lossy().into_owned(),
            candidate_path: candidate
                .candidate_path
                .map(|path| path.to_string_lossy().into_owned()),
            reason: candidate.reason,
            decision: candidate_decision_name(candidate.decision),
            discovered_at: candidate.discovered_at,
            decided_at: candidate.decided_at,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsolidationRequest {
    #[serde(default)]
    resolutions: Vec<ConsolidationResolutionRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsolidationResolutionRequest {
    field: String,
    choice: String,
}

#[derive(Debug, Serialize)]
struct ConsolidationPreflightResponse {
    tombstone_collection_id: i64,
    candidate_collection_id: i64,
    ready: bool,
    already_consolidated: bool,
    blockers: Vec<ConsolidationBlockerResponse>,
    conflicts: Vec<ConsolidationConflictResponse>,
}

#[derive(Debug, Serialize)]
struct ConsolidationBlockerResponse {
    kind: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct ConsolidationConflictResponse {
    field: &'static str,
    tombstone: ManualSelectionEvidenceResponse,
    candidate: ManualSelectionEvidenceResponse,
}

#[derive(Debug, Serialize)]
struct ManualSelectionEvidenceResponse {
    assertion_id: i64,
    source: &'static str,
    value: Value,
}

#[derive(Debug, Serialize)]
struct ConsolidationResponse {
    consolidation_id: i64,
    survivor_collection_id: i64,
    merged_collection_id: i64,
    already_completed: bool,
    resolutions: Value,
    consolidated_at: String,
}

impl TryFrom<ConsolidationPreflight> for ConsolidationPreflightResponse {
    type Error = ApiError;

    fn try_from(preflight: ConsolidationPreflight) -> Result<Self, Self::Error> {
        Ok(Self {
            tombstone_collection_id: preflight.tombstone_collection_id,
            candidate_collection_id: preflight.candidate_collection_id,
            ready: preflight.ready,
            already_consolidated: preflight.already_consolidated,
            blockers: preflight
                .blockers
                .into_iter()
                .map(|blocker| ConsolidationBlockerResponse {
                    kind: blocker.kind,
                    message: blocker.message,
                })
                .collect(),
            conflicts: preflight
                .conflicts
                .into_iter()
                .map(consolidation_conflict_response)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TryFrom<ConsolidationSnapshot> for ConsolidationResponse {
    type Error = ApiError;

    fn try_from(snapshot: ConsolidationSnapshot) -> Result<Self, Self::Error> {
        Ok(Self {
            consolidation_id: snapshot.consolidation_id,
            survivor_collection_id: snapshot.survivor_collection_id,
            merged_collection_id: snapshot.merged_collection_id,
            already_completed: snapshot.already_completed,
            resolutions: serde_json::from_str(&snapshot.resolutions_json)
                .map_err(|_| ApiError::internal())?,
            consolidated_at: snapshot.consolidated_at,
        })
    }
}

fn consolidation_conflict_response(
    conflict: ConsolidationConflict,
) -> Result<ConsolidationConflictResponse, ApiError> {
    Ok(ConsolidationConflictResponse {
        field: conflict.field.as_str(),
        tombstone: manual_selection_evidence_response(conflict.tombstone)?,
        candidate: manual_selection_evidence_response(conflict.candidate)?,
    })
}

fn manual_selection_evidence_response(
    evidence: ManualSelectionEvidence,
) -> Result<ManualSelectionEvidenceResponse, ApiError> {
    Ok(ManualSelectionEvidenceResponse {
        assertion_id: evidence.assertion_id,
        source: evidence.source.as_str(),
        value: serde_json::from_str(&evidence.value_json).map_err(|_| ApiError::internal())?,
    })
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

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ManualParodyRequest {
    Text(String),
    Detailed(ManualParodyDetails),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualParodyDetails {
    raw: String,
    canonical: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ManualClassificationRequest {
    Text(String),
    Detailed(ManualClassificationDetails),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualClassificationDetails {
    top_level: String,
    subcategory: Option<String>,
}

async fn decide_metadata_assertion<R>(
    State(state): State<HttpState<R>>,
    Path((collection_id, field, assertion_id)): Path<(String, String, String)>,
    payload: Result<Json<MetadataAssertionDecisionRequest>, JsonRejection>,
) -> Result<Json<MetadataHistoryResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let field = parse_metadata_field(&field)?;
    let assertion_id = parse_metadata_assertion_id(&assertion_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let decision = match payload.decision.as_str() {
        "select" => MetadataAssertionDecision::Select,
        "reject" => MetadataAssertionDecision::Reject,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_metadata_assertion_decision",
                "metadata assertion decision 必須是 select 或 reject",
            ));
        }
    };
    let history = tokio::task::spawn_blocking(move || {
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
            .decide_metadata_assertion(collection_id, field, assertion_id, decision)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(history.try_into()?))
}

async fn set_manual_metadata<R>(
    State(state): State<HttpState<R>>,
    Path((collection_id, field)): Path<(String, String)>,
    payload: Result<Json<ManualMetadataRequest>, JsonRejection>,
) -> Result<Json<CollectionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let field = parse_metadata_field(&field)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let value = decode_manual_metadata(field, payload.value)?;
    let collection = tokio::task::spawn_blocking(move || {
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
            .set_manual_metadata(collection_id, field, value)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(collection.into()))
}

async fn clear_manual_metadata<R>(
    State(state): State<HttpState<R>>,
    Path((collection_id, field)): Path<(String, String)>,
) -> Result<Json<CollectionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let field = parse_metadata_field(&field)?;
    let collection = tokio::task::spawn_blocking(move || {
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
            .clear_manual_metadata(collection_id, field)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(collection.into()))
}

async fn add_collection_tag<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    payload: Result<Json<TagRequest>, JsonRejection>,
) -> Result<Json<CollectionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection = tokio::task::spawn_blocking(move || {
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
            .add_collection_tag(collection_id, &payload.name)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(collection.into()))
}

async fn batch_add_collection_tag<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<BatchTagRequest>, JsonRejection>,
) -> Result<Json<BatchMutationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validate_batch_collection_ids(payload.collection_ids)?;
    let name = payload.name.trim().to_owned();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_tag_name",
            "tag name 不得為空白",
        ));
    }
    let report = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        Ok::<_, ApiError>(application.batch_add_collection_tag(&collection_ids, &name))
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(batch_mutation_response(report)))
}

async fn batch_set_manual_metadata<R>(
    State(state): State<HttpState<R>>,
    Path(field): Path<String>,
    payload: Result<Json<BatchMetadataRequest>, JsonRejection>,
) -> Result<Json<BatchMutationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let field = parse_metadata_field(&field)?;
    if !matches!(field, MetadataField::Parody | MetadataField::Classification) {
        return Err(ApiError::bad_request(
            "unsupported_batch_metadata_field",
            "批次 metadata 目前只支援 parody 與 classification",
        ));
    }
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validate_batch_collection_ids(payload.collection_ids)?;
    let value = decode_manual_metadata(field, payload.value)?;
    let report = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        Ok::<_, ApiError>(application.batch_set_manual_metadata(&collection_ids, field, value))
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(batch_mutation_response(report)))
}

fn validate_batch_collection_ids(collection_ids: Vec<i64>) -> Result<Vec<i64>, ApiError> {
    if collection_ids.is_empty() || collection_ids.len() > 1_000 {
        return Err(ApiError::bad_request(
            "invalid_batch_collection_ids",
            "collection_ids 必須包含 1 到 1000 個 ID",
        ));
    }
    if collection_ids
        .iter()
        .any(|collection_id| *collection_id <= 0)
    {
        return Err(ApiError::bad_request(
            "invalid_batch_collection_ids",
            "collection_ids 必須全部是正整數",
        ));
    }
    let mut seen = HashSet::new();
    Ok(collection_ids
        .into_iter()
        .filter(|collection_id| seen.insert(*collection_id))
        .collect())
}

fn batch_mutation_response(report: ApplicationBatchReport) -> BatchMutationResponse {
    let mut succeeded = 0;
    let mut unchanged = 0;
    let mut failed = 0;
    let items = report
        .items
        .into_iter()
        .map(|item| match item.outcome {
            ApplicationBatchOutcome::Succeeded(collection) => {
                succeeded += 1;
                BatchMutationItemResponse {
                    collection_id: item.collection_id,
                    status: "succeeded",
                    collection: Some(collection.into()),
                    error: None,
                }
            }
            ApplicationBatchOutcome::Unchanged(collection) => {
                unchanged += 1;
                BatchMutationItemResponse {
                    collection_id: item.collection_id,
                    status: "unchanged",
                    collection: Some(collection.into()),
                    error: None,
                }
            }
            ApplicationBatchOutcome::Failed(error) => {
                failed += 1;
                let error = ApiError::from_application(error);
                BatchMutationItemResponse {
                    collection_id: item.collection_id,
                    status: "failed",
                    collection: None,
                    error: Some(BatchMutationErrorResponse {
                        code: error.code,
                        message: error.message,
                    }),
                }
            }
        })
        .collect::<Vec<_>>();
    BatchMutationResponse {
        summary: BatchMutationSummaryResponse {
            total: items.len(),
            completed: items.len(),
            succeeded,
            unchanged,
            failed,
        },
        items,
    }
}

async fn remove_collection_tag<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    payload: Result<Json<TagRequest>, JsonRejection>,
) -> Result<Json<CollectionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection = tokio::task::spawn_blocking(move || {
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
            .remove_collection_tag(collection_id, &payload.name)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(collection.into()))
}

async fn enqueue_external_search<R>(
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

async fn get_external_search_job<R>(
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

async fn preflight_external_search_batch<R>(
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

async fn create_external_search_batch<R>(
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

async fn get_external_search_batch<R>(
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

async fn retry_external_search_batch<R>(
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

async fn list_tombstone_candidates<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<TombstoneCandidatesResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let candidates = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .tombstone_candidates()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(TombstoneCandidatesResponse {
        items: candidates.into_iter().map(Into::into).collect(),
    }))
}

async fn decide_tombstone_candidate<R>(
    State(state): State<HttpState<R>>,
    Path((tombstone_id, candidate_id)): Path<(String, String)>,
    payload: Result<Json<TombstoneCandidateDecisionRequest>, JsonRejection>,
) -> Result<Json<TombstoneCandidateResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let tombstone_id = parse_tombstone_id(&tombstone_id)?;
    let candidate_id = parse_candidate_id(&candidate_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let decision = match payload.decision.as_str() {
        "confirmed" => CandidateDecision::Confirmed,
        "rejected" => CandidateDecision::Rejected,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_tombstone_candidate_decision",
                "tombstone candidate decision 必須是 confirmed 或 rejected",
            ));
        }
    };
    let candidate = tokio::task::spawn_blocking(move || {
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
            .decide_tombstone_candidate(tombstone_id, candidate_id, decision)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(candidate.into()))
}

async fn consolidation_preflight<R>(
    State(state): State<HttpState<R>>,
    Path((tombstone_id, candidate_id)): Path<(String, String)>,
) -> Result<Json<ConsolidationPreflightResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let tombstone_id = parse_tombstone_id(&tombstone_id)?;
    let candidate_id = parse_candidate_id(&candidate_id)?;
    let preflight = tokio::task::spawn_blocking(move || {
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
        application
            .consolidation_preflight(tombstone_id, candidate_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(preflight.try_into()?))
}

async fn consolidate_tombstone_candidate<R>(
    State(state): State<HttpState<R>>,
    Path((tombstone_id, candidate_id)): Path<(String, String)>,
    payload: Result<Json<ConsolidationRequest>, JsonRejection>,
) -> Result<Json<ConsolidationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let tombstone_id = parse_tombstone_id(&tombstone_id)?;
    let candidate_id = parse_candidate_id(&candidate_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let resolutions = payload
        .resolutions
        .into_iter()
        .map(parse_consolidation_resolution)
        .collect::<Result<Vec<_>, _>>()?;
    let consolidated = tokio::task::spawn_blocking(move || {
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
            .consolidate_tombstone_candidate(tombstone_id, candidate_id, &resolutions)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(consolidated.try_into()?))
}

fn parse_consolidation_resolution(
    resolution: ConsolidationResolutionRequest,
) -> Result<ConsolidationResolution, ApiError> {
    let field = parse_metadata_field(&resolution.field).map_err(|_| {
        ApiError::bad_request(
            "invalid_consolidation_resolution",
            "consolidation resolution 包含不支援的 metadata field",
        )
    })?;
    let choice = match resolution.choice.as_str() {
        "tombstone" => ConsolidationChoice::Tombstone,
        "candidate" => ConsolidationChoice::Candidate,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_consolidation_resolution",
                "consolidation choice 必須是 tombstone 或 candidate",
            ));
        }
    };
    Ok(ConsolidationResolution { field, choice })
}

fn parse_collection_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_collection_id", "collection ID 必須是正整數"))
}

fn parse_work_basket_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_work_basket_id", "工作籃 ID 必須是正整數"))
}

fn parse_saved_view_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_saved_view_id", "Saved View ID 必須是正整數"))
}

fn parse_tombstone_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_tombstone_id", "tombstone ID 必須是正整數"))
}

fn parse_candidate_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_candidate_id", "candidate ID 必須是正整數"))
}

fn candidate_decision_name(decision: CandidateDecision) -> &'static str {
    match decision {
        CandidateDecision::Pending => "pending",
        CandidateDecision::Confirmed => "confirmed",
        CandidateDecision::Rejected => "rejected",
    }
}

fn parse_metadata_assertion_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_metadata_assertion_id",
                "metadata assertion ID 必須是正整數",
            )
        })
}

fn parse_external_search_job_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_external_search_job_id",
                "external search job ID 必須是正整數",
            )
        })
}

fn parse_external_search_batch_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_external_search_batch_id",
                "external search batch ID 必須是正整數",
            )
        })
}

fn parse_external_search_batch_strategy(
    value: &str,
) -> Result<ExternalSearchBatchStrategy, ApiError> {
    match value {
        "only_missing" => Ok(ExternalSearchBatchStrategy::OnlyMissing),
        "specified" => Ok(ExternalSearchBatchStrategy::Specified),
        _ => Err(ApiError::bad_request(
            "invalid_external_search_batch_strategy",
            "external search batch strategy 必須是 only_missing 或 specified",
        )),
    }
}

fn parse_external_search_fields(values: Vec<String>) -> Result<Vec<MetadataField>, ApiError> {
    if values.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_external_search_fields",
            "external search 至少必須指定一個 metadata field",
        ));
    }
    let mut fields = Vec::with_capacity(values.len());
    for value in values {
        let field = parse_metadata_field(&value).map_err(|_| {
            ApiError::bad_request(
                "invalid_external_search_fields",
                "external search 包含不支援的 metadata field",
            )
        })?;
        if !fields.contains(&field) {
            fields.push(field);
        }
    }
    Ok(fields)
}

fn parse_metadata_field(value: &str) -> Result<MetadataField, ApiError> {
    match value {
        "title" => Ok(MetadataField::Title),
        "event" => Ok(MetadataField::Event),
        "circle" => Ok(MetadataField::Circle),
        "authors" => Ok(MetadataField::Authors),
        "parody" => Ok(MetadataField::Parody),
        "classification" => Ok(MetadataField::Classification),
        "is_dl" => Ok(MetadataField::IsDl),
        _ => Err(ApiError::bad_request(
            "invalid_metadata_field",
            "不支援指定的 metadata field",
        )),
    }
}

fn decode_manual_metadata(field: MetadataField, value: Value) -> Result<MetadataValue, ApiError> {
    let invalid =
        || ApiError::bad_request("invalid_metadata_value", "metadata value 的型別或內容無效");
    match field {
        MetadataField::Title | MetadataField::Event | MetadataField::Circle => {
            let value = serde_json::from_value::<String>(value).map_err(|_| invalid())?;
            Ok(MetadataValue::Text(nonempty_metadata_text(value)?))
        }
        MetadataField::Authors => {
            let values = serde_json::from_value::<Vec<String>>(value).map_err(|_| invalid())?;
            let values = values
                .into_iter()
                .map(nonempty_metadata_text)
                .collect::<Result<Vec<_>, _>>()?;
            if values.is_empty() {
                return Err(invalid());
            }
            Ok(MetadataValue::Authors(Authors { raw: None, values }))
        }
        MetadataField::Parody => {
            let value =
                serde_json::from_value::<ManualParodyRequest>(value).map_err(|_| invalid())?;
            let (raw, canonical) = match value {
                ManualParodyRequest::Text(value) => {
                    let value = nonempty_metadata_text(value)?;
                    (value.clone(), value)
                }
                ManualParodyRequest::Detailed(value) => {
                    let raw = nonempty_metadata_text(value.raw)?;
                    let canonical = value
                        .canonical
                        .map(nonempty_metadata_text)
                        .transpose()?
                        .unwrap_or_else(|| raw.clone());
                    (raw, canonical)
                }
            };
            Ok(MetadataValue::Parody(Parody {
                raw,
                canonical,
                evidence: "manual".to_owned(),
            }))
        }
        MetadataField::Classification => {
            let value = serde_json::from_value::<ManualClassificationRequest>(value)
                .map_err(|_| invalid())?;
            let (top_level, subcategory) = match value {
                ManualClassificationRequest::Text(value) => (nonempty_metadata_text(value)?, None),
                ManualClassificationRequest::Detailed(value) => (
                    nonempty_metadata_text(value.top_level)?,
                    value
                        .subcategory
                        .map(|value| value.trim().to_owned())
                        .filter(|value| !value.is_empty()),
                ),
            };
            Ok(MetadataValue::Classification(Classification {
                top_level,
                subcategory,
                raw_marker: None,
            }))
        }
        MetadataField::IsDl => Ok(MetadataValue::Boolean(
            serde_json::from_value::<bool>(value).map_err(|_| invalid())?,
        )),
    }
}

fn nonempty_metadata_text(value: String) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        Err(ApiError::bad_request(
            "invalid_metadata_value",
            "metadata value 不得為空白；清除手動值請使用 DELETE",
        ))
    } else {
        Ok(value.to_owned())
    }
}

fn positive_u32_or(value: Option<i64>, fallback: u32) -> u32 {
    value
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

fn clamped_per_page(value: Option<i64>) -> u32 {
    value.map(|value| value.clamp(1, 200) as u32).unwrap_or(50)
}

fn parse_facet_query(raw_query: Option<&str>) -> Result<(CollectionFacet, String, u32), ApiError> {
    let mut field = None;
    let mut search = String::new();
    let mut limit = 20;
    let mut scalar_keys = HashSet::new();
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        let key = key.as_ref();
        let value = value.as_ref();
        match key {
            "field" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                field = Some(match value {
                    "event" => CollectionFacet::Event,
                    "circle" => CollectionFacet::Circle,
                    "author" => CollectionFacet::Author,
                    "parody" => CollectionFacet::Parody,
                    "tag" => CollectionFacet::Tag,
                    _ => {
                        return Err(ApiError::bad_request(
                            "invalid_facet_field",
                            "field 必須是 event、circle、author、parody 或 tag",
                        ));
                    }
                });
            }
            "q" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                search = value.trim().to_owned();
            }
            "limit" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                let parsed = value.parse::<i64>().map_err(|_| {
                    ApiError::bad_request("invalid_facet_limit", "limit 必須是整數")
                })?;
                limit = parsed.clamp(1, 50) as u32;
            }
            _ => {}
        }
    }
    let field = field
        .ok_or_else(|| ApiError::bad_request("missing_facet_field", "facet 查詢必須指定 field"))?;
    Ok((field, search, limit))
}

fn parse_vocabulary_query(raw_query: Option<&str>) -> Result<Option<VocabularyField>, ApiError> {
    let mut field = None;
    let mut scalar_keys = HashSet::new();
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        if key == "field" {
            ensure_single_parameter(&mut scalar_keys, "field")?;
            field = Some(parse_vocabulary_field(&value)?);
        }
    }
    Ok(field)
}

fn parse_vocabulary_field(value: &str) -> Result<VocabularyField, ApiError> {
    VocabularyField::parse(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_vocabulary_field",
            "field 必須是 event、circle、author 或 parody",
        )
    })
}

fn parse_collection_query(raw_query: Option<&str>) -> Result<CollectionQuery, ApiError> {
    let mut query = CollectionQuery::default();
    let mut scalar_keys = HashSet::new();
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        let key = key.as_ref();
        let value = value.as_ref();
        match key {
            "q" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.search = Some(value.to_owned());
            }
            "page" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                let value = value.parse::<i64>().map_err(|_| invalid_query())?;
                query.page = positive_u32_or(Some(value), 1);
            }
            "per_page" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                let value = value.parse::<i64>().map_err(|_| invalid_query())?;
                query.per_page = clamped_per_page(Some(value));
            }
            "sort" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.sort = match value {
                    "created" => CollectionSort::Created,
                    "updated" => CollectionSort::Updated,
                    "title" => CollectionSort::Title,
                    _ => CollectionSort::default(),
                };
            }
            "direction" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.direction = match value {
                    "asc" => SortDirection::Ascending,
                    "desc" => SortDirection::Descending,
                    _ => SortDirection::default(),
                };
            }
            "event" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.event = Some(required_filter_value(value)?);
            }
            "circle" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.circle = Some(required_filter_value(value)?);
            }
            "author" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.author = Some(required_filter_value(value)?);
            }
            "parody" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.parody = Some(required_filter_value(value)?);
            }
            "classification" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.classification = Some(required_filter_value(value)?);
            }
            "subcategory" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.subcategory = Some(required_filter_value(value)?);
            }
            "source" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.source = Some(match value {
                    "archive" => SourceKind::Archive,
                    "downloads" => SourceKind::Downloads,
                    _ => {
                        return Err(ApiError::bad_request(
                            "invalid_collection_filter",
                            "source 必須是 archive 或 downloads",
                        ));
                    }
                });
            }
            "tag" => query.filters.tags.push(required_filter_value(value)?),
            "untagged" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.untagged = match value {
                    "1" | "true" => true,
                    "" | "0" | "false" => false,
                    _ => {
                        return Err(ApiError::bad_request(
                            "invalid_collection_filter",
                            "untagged 必須是 1、0、true 或 false",
                        ));
                    }
                };
            }
            "missing" => query.filters.missing.push(match value {
                "any" => MissingMetadataField::Any,
                "title" => MissingMetadataField::Title,
                "event" => MissingMetadataField::Event,
                "circle" => MissingMetadataField::Circle,
                "authors" => MissingMetadataField::Authors,
                "parody" => MissingMetadataField::Parody,
                "classification" => MissingMetadataField::Classification,
                _ => {
                    return Err(ApiError::bad_request(
                        "invalid_collection_filter",
                        "missing 必須是 any、title、event、circle、authors、parody 或 classification",
                    ));
                }
            }),
            _ => {}
        }
    }
    Ok(query)
}

fn parse_review_queue_query(raw_query: Option<&str>) -> Result<ReviewQueueQuery, ApiError> {
    let mut query = ReviewQueueQuery::default();
    let mut scalar_keys = HashSet::new();
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        let key = key.as_ref();
        let value = value.as_ref();
        match key {
            "page" => {
                ensure_single_review_parameter(&mut scalar_keys, key)?;
                let value = value.parse::<i64>().map_err(|_| invalid_review_query())?;
                query.page = positive_u32_or(Some(value), 1);
            }
            "per_page" => {
                ensure_single_review_parameter(&mut scalar_keys, key)?;
                let value = value.parse::<i64>().map_err(|_| invalid_review_query())?;
                query.per_page = value.clamp(1, 100) as u32;
            }
            "kind" => {
                ensure_single_review_parameter(&mut scalar_keys, key)?;
                query.kind = match value {
                    "all" => ReviewQueueKind::All,
                    "missing" => ReviewQueueKind::Missing,
                    "candidate" => ReviewQueueKind::Candidate,
                    _ => return Err(invalid_review_query()),
                };
            }
            _ => {
                return Err(invalid_review_query());
            }
        }
    }
    Ok(query)
}

fn ensure_single_review_parameter(
    scalar_keys: &mut HashSet<String>,
    key: &str,
) -> Result<(), ApiError> {
    if scalar_keys.insert(key.to_owned()) {
        Ok(())
    } else {
        Err(invalid_review_query())
    }
}

fn ensure_single_parameter(scalar_keys: &mut HashSet<String>, key: &str) -> Result<(), ApiError> {
    if scalar_keys.insert(key.to_owned()) {
        Ok(())
    } else {
        Err(invalid_query())
    }
}

fn required_filter_value(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        Err(ApiError::bad_request(
            "invalid_collection_filter",
            "collection filter value 不得為空白",
        ))
    } else {
        Ok(value.to_owned())
    }
}

fn invalid_query() -> ApiError {
    ApiError::bad_request("invalid_query", "collection query 參數無效")
}

fn invalid_review_query() -> ApiError {
    ApiError::bad_request(
        "invalid_review_query",
        "review queue 只支援 page、per_page 與 kind=all|missing|candidate",
    )
}

fn source_name(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Archive => "archive",
        SourceKind::Downloads => "downloads",
    }
}

fn collection_sort_name(sort: CollectionSort) -> &'static str {
    match sort {
        CollectionSort::Created => "created",
        CollectionSort::Updated => "updated",
        CollectionSort::Title => "title",
    }
}

fn sort_direction_name(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Ascending => "asc",
        SortDirection::Descending => "desc",
    }
}

fn missing_name(field: MissingMetadataField) -> &'static str {
    match field {
        MissingMetadataField::Any => "any",
        MissingMetadataField::Title => "title",
        MissingMetadataField::Event => "event",
        MissingMetadataField::Circle => "circle",
        MissingMetadataField::Authors => "authors",
        MissingMetadataField::Parody => "parody",
        MissingMetadataField::Classification => "classification",
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterLibraryRootRequest {
    path: String,
    source: String,
    label: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateLibraryRootRequest {
    path: String,
    source: String,
    label: String,
}

#[derive(Debug, Serialize)]
struct LibraryRootsResponse {
    roots: Vec<LibraryRootResponse>,
}

#[derive(Debug, Serialize)]
struct LibraryRootResponse {
    id: i64,
    path: String,
    source: &'static str,
    label: String,
    active: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveCollectionsRequest {
    collection_ids: Vec<i64>,
    archive_root_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteCollectionsRequest {
    collection_ids: Vec<i64>,
    mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenamePreflightRequest {
    collection_ids: Vec<i64>,
    template: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameCollectionsRequest {
    template: String,
    items: Vec<RenameExpectedItem>,
}

#[derive(Debug, Serialize)]
struct FileActionBatchResponse {
    succeeded: usize,
    failed: usize,
    pending_recovery: usize,
    items: Vec<FileActionItemResponse>,
}

#[derive(Debug, Serialize)]
struct FileActionItemResponse {
    collection_id: i64,
    operation_id: Option<i64>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl From<BatchReport> for FileActionBatchResponse {
    fn from(report: BatchReport) -> Self {
        Self {
            succeeded: report.succeeded(),
            failed: report.failed(),
            pending_recovery: report.pending_recovery(),
            items: report
                .items
                .into_iter()
                .map(|item| FileActionItemResponse {
                    collection_id: item.collection_id,
                    operation_id: item.operation_id,
                    status: match item.status {
                        ItemStatus::Succeeded => "succeeded",
                        ItemStatus::Failed => "failed",
                        ItemStatus::PendingRecovery => "pending_recovery",
                    },
                    error: item.error,
                })
                .collect(),
        }
    }
}

async fn move_collections<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<MoveCollectionsRequest>, JsonRejection>,
) -> Result<Json<FileActionBatchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validated_collection_ids(payload.collection_ids)?;
    let archive_root_id = positive_id(
        payload.archive_root_id,
        "invalid_archive_root_id",
        "archive root ID 必須是正整數",
    )?;
    let report = tokio::task::spawn_blocking(move || {
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
        Ok(application.move_collections_to_archive(&collection_ids, archive_root_id))
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(report.into()))
}

async fn preflight_rename_collections<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<RenamePreflightRequest>, JsonRejection>,
) -> Result<Json<RenamePreflight>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validated_collection_ids(payload.collection_ids)?;
    let template = payload.template;
    if template.trim().is_empty() {
        return Err(ApiError::bad_request(
            "invalid_rename_template",
            "rename template 不得為空",
        ));
    }
    let preflight = tokio::task::spawn_blocking(move || {
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
        application
            .rename_preflight(&collection_ids, &template)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(preflight))
}

async fn rename_collections<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<RenameCollectionsRequest>, JsonRejection>,
) -> Result<Json<FileActionBatchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    if payload.template.trim().is_empty() || payload.items.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_rename_request",
            "rename template 與 preflight items 不得為空",
        ));
    }
    validated_collection_ids(
        payload
            .items
            .iter()
            .map(|item| item.collection_id)
            .collect(),
    )?;
    let template = payload.template;
    let items = payload.items;
    let report = tokio::task::spawn_blocking(move || {
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
            .apply_rename_preflight(&template, &items)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(report.into()))
}

async fn delete_collections<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<DeleteCollectionsRequest>, JsonRejection>,
) -> Result<Json<FileActionBatchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validated_collection_ids(payload.collection_ids)?;
    let mode = match payload.mode.as_str() {
        "soft" => DeleteMode::Soft,
        "permanent" => DeleteMode::Permanent,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_delete_mode",
                "delete mode 必須是 soft 或 permanent",
            ));
        }
    };
    let requests: Vec<_> = collection_ids
        .into_iter()
        .map(|collection_id| DeleteRequest {
            collection_id,
            mode,
        })
        .collect();
    let report = tokio::task::spawn_blocking(move || {
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
        Ok(application.delete_collections(&requests))
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(report.into()))
}

fn validated_collection_ids(collection_ids: Vec<i64>) -> Result<Vec<i64>, ApiError> {
    if collection_ids.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_collection_ids",
            "collection_ids 不得為空",
        ));
    }
    let mut unique = HashSet::with_capacity(collection_ids.len());
    if collection_ids
        .iter()
        .any(|collection_id| *collection_id <= 0 || !unique.insert(*collection_id))
    {
        return Err(ApiError::bad_request(
            "invalid_collection_ids",
            "collection_ids 必須是互不重複的正整數",
        ));
    }
    Ok(collection_ids)
}

fn positive_id(value: i64, code: &'static str, message: &str) -> Result<i64, ApiError> {
    if value > 0 {
        Ok(value)
    } else {
        Err(ApiError::bad_request(code, message))
    }
}

impl From<LibraryRootSnapshot> for LibraryRootResponse {
    fn from(root: LibraryRootSnapshot) -> Self {
        Self {
            id: root.id,
            path: root.path.to_string_lossy().into_owned(),
            source: source_name(root.source),
            label: root.label,
            active: root.active,
            created_at: root.created_at,
            updated_at: root.updated_at,
        }
    }
}

async fn list_library_roots<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<LibraryRootsResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let roots = tokio::task::spawn_blocking(move || {
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
        application
            .library_roots()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(LibraryRootsResponse {
        roots: roots.into_iter().map(Into::into).collect(),
    }))
}

async fn register_library_root<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<RegisterLibraryRootRequest>, JsonRejection>,
) -> Result<Json<LibraryRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let source = parse_library_root_source(&payload.source)?;
    let path = PathBuf::from(payload.path);
    let label = payload.label.trim().to_owned();
    let root = tokio::task::spawn_blocking(move || {
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
            .register_library_root(&path, source, &label)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

async fn update_library_root<R>(
    State(state): State<HttpState<R>>,
    Path(root_id): Path<String>,
    payload: Result<Json<UpdateLibraryRootRequest>, JsonRejection>,
) -> Result<Json<LibraryRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let root_id = parse_library_root_id(root_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let source = parse_library_root_source(&payload.source)?;
    let path = PathBuf::from(payload.path);
    let label = payload.label.trim().to_owned();
    let root = tokio::task::spawn_blocking(move || {
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
            .update_library_root(root_id, &path, source, &label)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

async fn reactivate_library_root<R>(
    State(state): State<HttpState<R>>,
    Path(root_id): Path<String>,
) -> Result<Json<LibraryRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let root_id = parse_library_root_id(root_id)?;
    let root = tokio::task::spawn_blocking(move || {
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
            .reactivate_library_root(root_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

async fn deactivate_library_root<R>(
    State(state): State<HttpState<R>>,
    Path(root_id): Path<String>,
) -> Result<Json<LibraryRootResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let root_id = parse_library_root_id(root_id)?;
    let root = tokio::task::spawn_blocking(move || {
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
            .deactivate_library_root(root_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(root.into()))
}

fn parse_library_root_id(root_id: String) -> Result<i64, ApiError> {
    root_id
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ApiError::bad_request("invalid_library_root_id", "library root ID 必須是正整數")
        })
}

fn parse_library_root_source(source: &str) -> Result<SourceKind, ApiError> {
    match source {
        "archive" => Ok(SourceKind::Archive),
        "downloads" => Ok(SourceKind::Downloads),
        _ => Err(ApiError::bad_request(
            "invalid_library_root_source",
            "library root source 必須是 archive 或 downloads",
        )),
    }
}

async fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "route_not_found",
        "找不到指定的 API route",
    )
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "此 API route 不支援指定的 HTTP method",
    )
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartScanRequest {
    mode: Option<String>,
    expected: Option<ApplicationScanExpectation>,
}

async fn preflight_scan<R>(
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

async fn start_scan<R>(
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
struct LatestScanEnvelope {
    scan: Option<ScanRunResponse>,
}

async fn get_latest_scan<R>(
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

async fn get_scan<R>(
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
struct ScanRunResponse {
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
struct ScanIssueResponse {
    id: i64,
    path: String,
    kind: String,
    message: String,
}

async fn request_guard(request: Request<Body>, next: Next) -> Response {
    if !request
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_allowed_loopback_authority)
    {
        return ApiError::new(
            StatusCode::MISDIRECTED_REQUEST,
            "invalid_host",
            "HTTP Host 必須是 localhost loopback",
        )
        .into_response();
    }
    if is_mutating(request.method())
        && let Some(value) = origin_or_referer(request.headers())
        && !value.to_str().ok().is_some_and(is_allowed_loopback_origin)
    {
        return ApiError::forbidden(
            "cross_site_write_rejected",
            "寫入要求的 Origin／Referer 不是 localhost loopback",
        )
        .into_response();
    }
    next.run(request).await
}

fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn origin_or_referer(headers: &HeaderMap) -> Option<&axum::http::HeaderValue> {
    headers
        .get(axum::http::header::ORIGIN)
        .or_else(|| headers.get(axum::http::header::REFERER))
}

fn is_allowed_loopback_origin(value: &str) -> bool {
    let Ok(uri) = Uri::from_str(value) else {
        return false;
    };
    if uri.scheme().is_none() || uri.authority().is_none() {
        return false;
    }
    let Some(host) = uri.host() else {
        return false;
    };
    is_allowed_loopback_host(host)
}

fn is_allowed_loopback_authority(value: &str) -> bool {
    Authority::from_str(value)
        .ok()
        .is_some_and(|authority| is_allowed_loopback_host(authority.host()))
}

fn is_allowed_loopback_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']);
    host.eq_ignore_ascii_case("localhost")
        || IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    merged_into_collection_id: Option<i64>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    merged_into_collection_id: Option<i64>,
}

impl ApiError {
    fn bad_request(code: &'static str, message: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn forbidden(code: &'static str, message: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    fn conflict(code: &'static str, message: &str) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    fn unavailable(code: &'static str, message: &str) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, code, message)
    }

    fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "伺服器內部錯誤",
        )
    }

    fn merged(survivor_id: i64) -> Self {
        Self {
            status: StatusCode::GONE,
            code: "collection_merged",
            message: format!("收藏已合併至 collection {survivor_id}"),
            merged_into_collection_id: Some(survivor_id),
        }
    }

    fn new(status: StatusCode, code: &'static str, message: &str) -> Self {
        Self {
            status,
            code,
            message: message.to_owned(),
            merged_into_collection_id: None,
        }
    }

    fn from_application(error: ApplicationError) -> Self {
        match error {
            ApplicationError::Storage(error) => Self::from_storage(error),
            ApplicationError::ThumbnailNotConfigured => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "thumbnail_not_configured",
                "thumbnail 服務尚未設定",
            ),
            ApplicationError::InvalidSettings(reason) => {
                Self::bad_request("invalid_settings", &reason)
            }
            ApplicationError::Thumbnail(_)
            | ApplicationError::ThumbnailCacheIo(_)
            | ApplicationError::Json(_) => Self::internal(),
        }
    }

    fn from_cover_application(error: ApplicationError) -> Self {
        match error {
            ApplicationError::Thumbnail(error) => {
                Self::bad_request("invalid_cover_candidate", &error.message)
            }
            ApplicationError::InvalidSettings(reason) => {
                Self::conflict("cover_source_changed", &reason)
            }
            other => Self::from_application(other),
        }
    }

    fn from_launch(error: LaunchError) -> Self {
        match error {
            LaunchError::Storage(error) => Self::from_storage(error),
            LaunchError::CollectionFileNotFound => Self::new(
                StatusCode::NOT_FOUND,
                "collection_file_not_found",
                "收藏檔案不存在",
            ),
            LaunchError::InvalidCollectionFile(reason) => {
                Self::conflict("invalid_collection_file", &reason)
            }
            LaunchError::ReaderNotConfigured => Self::new(
                StatusCode::NOT_FOUND,
                "reader_not_configured",
                "尚未設定閱讀器",
            ),
            LaunchError::ReaderUnavailable => Self::new(
                StatusCode::NOT_FOUND,
                "reader_not_found",
                "設定的閱讀器不存在或不是一般檔案",
            ),
            LaunchError::Launcher { .. } => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "external_launch_failed",
                "無法啟動外部程式",
            ),
        }
    }

    fn from_storage(error: StorageError) -> Self {
        match error {
            StorageError::LibraryRootNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "library_root_not_found",
                "找不到指定的 library root",
            ),
            StorageError::InvalidLibraryRoot(reason) => {
                Self::bad_request("invalid_library_root", &reason)
            }
            StorageError::InvalidMetadata(reason) => {
                Self::bad_request("invalid_metadata_value", &reason)
            }
            StorageError::CollectionNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "collection_not_found",
                "找不到指定的 collection",
            ),
            StorageError::AssertionUnavailable(_) => Self::conflict(
                "metadata_assertion_unavailable",
                "metadata assertion 不存在、欄位歸屬不符，或已不可裁決",
            ),
            StorageError::ExternalSearchJobNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "external_search_job_not_found",
                "找不到指定的 external search job",
            ),
            StorageError::ExternalSearchJobUnavailable(_) => Self::conflict(
                "external_search_job_unavailable",
                "external search job 目前不可執行",
            ),
            StorageError::InvalidExternalSearchJob(reason) => {
                Self::bad_request("invalid_external_search_job", &reason)
            }
            StorageError::ExternalSearchBatchNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "external_search_batch_not_found",
                "找不到指定的 external search batch",
            ),
            StorageError::InvalidExternalSearchBatch(reason) => {
                Self::bad_request("invalid_external_search_batch", &reason)
            }
            StorageError::ThumbnailStateNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "thumbnail_state_not_found",
                "找不到指定的 thumbnail state",
            ),
            StorageError::ThumbnailStateUnavailable(_) => Self::conflict(
                "thumbnail_state_unavailable",
                "thumbnail state 目前不可執行",
            ),
            StorageError::InvalidThumbnailState(reason) => {
                Self::bad_request("invalid_thumbnail_state", &reason)
            }
            StorageError::InvalidApplicationSettings(reason) => {
                Self::bad_request("invalid_settings", &reason)
            }
            StorageError::SavedViewNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "saved_view_not_found",
                "找不到指定的 Saved View",
            ),
            StorageError::SavedViewNameConflict(_) => {
                Self::conflict("saved_view_name_conflict", "Saved View 名稱已存在")
            }
            StorageError::InvalidSavedView(reason) => {
                Self::bad_request("invalid_saved_view", &reason)
            }
            StorageError::WorkBasketNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "work_basket_not_found",
                "找不到指定的工作籃",
            ),
            StorageError::InvalidCanonicalMapping(reason) => {
                Self::bad_request("invalid_vocabulary_action", &reason)
            }
            StorageError::ScanAlreadyRunning => {
                Self::conflict("scan_already_running", "重新掃描正在執行")
            }
            StorageError::ScanRunNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "scan_run_not_found",
                "找不到指定的 scan run",
            ),
            StorageError::InvalidScanRun(reason) => Self::bad_request("invalid_scan_run", &reason),
            StorageError::InvalidLifecycle(reason) => Self::conflict("invalid_lifecycle", &reason),
            _ => Self::internal(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                    merged_into_collection_id: self.merged_into_collection_id,
                },
            }),
        )
            .into_response()
    }
}
