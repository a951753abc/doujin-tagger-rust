//! Thin localhost-only HTTP adapter for the Rust application service.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::middleware::{self};
use axum::routing::{get, patch, post, put};
use doujin_app::ApplicationService;
use doujin_files::RecycleBin;
use serde::Serialize;
use tokio::net::TcpListener;

mod collections;
mod consolidation;
mod covers;
mod duplicates;
mod error;
mod exports;
mod external_search;
mod files;
mod frontend;
mod guard;
mod instance;
mod metadata;
mod params;
mod roots;
mod saved_views;
mod scan;
mod service;
mod settings;
mod statistics;
mod thumbnails;
mod vocabulary;
mod work_baskets;

pub use instance::ServiceInstanceConfig;
pub use service::{ServiceOptions, run_service};

use crate::error::ApiError;

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
    serve_shared_with_shutdown_and_instance(listener, application, None, shutdown).await
}

/// 與 [`serve_shared_with_shutdown`] 相同，額外帶入 `instance_id` 供 `/api/health` 回報。
pub(crate) async fn serve_shared_with_shutdown_and_instance<R, F>(
    listener: TcpListener,
    application: SharedApplication<R>,
    instance_id: Option<String>,
    shutdown: F,
) -> Result<(), ServerError>
where
    R: RecycleBin + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    validate_loopback_address(listener.local_addr()?)?;
    axum::serve(listener, build_router(application, instance_id))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

fn build_router<R>(application: SharedApplication<R>, instance_id: Option<String>) -> Router
where
    R: RecycleBin + Send + 'static,
{
    let state = HttpState {
        application,
        thumbnail_cache_jobs: Arc::new(Mutex::new(ThumbnailCacheJobs::default())),
        instance_id,
    };
    Router::new()
        .route("/", get(frontend::frontend_index))
        .route("/assets/app.css", get(frontend::frontend_css))
        .route("/assets/app.js", get(frontend::frontend_javascript))
        .route("/api/health", get(health::<R>))
        .route(
            "/api/settings",
            get(settings::get_settings::<R>).put(settings::update_settings::<R>),
        )
        .route("/api/stats", get(statistics::get_statistics::<R>))
        .route("/api/facets", get(statistics::get_facets::<R>))
        .route(
            "/api/duplicate-jobs",
            post(duplicates::start_duplicate_scan::<R>),
        )
        .route(
            "/api/duplicate-jobs/current",
            get(duplicates::get_current_duplicate_scan::<R>),
        )
        .route(
            "/api/duplicate-jobs/{job_id}",
            get(duplicates::get_duplicate_scan::<R>),
        )
        .route(
            "/api/duplicate-jobs/{job_id}/failures",
            get(duplicates::get_duplicate_scan_failures::<R>),
        )
        .route(
            "/api/duplicate-jobs/{job_id}/retry-failures",
            post(duplicates::retry_duplicate_scan_failures::<R>),
        )
        .route(
            "/api/duplicates",
            get(duplicates::list_duplicate_candidates::<R>),
        )
        .route(
            "/api/duplicates/{left_collection_id}/{right_collection_id}/exclude",
            post(duplicates::exclude_duplicate_pair::<R>),
        )
        .route(
            "/api/duplicates/{left_collection_id}/{right_collection_id}/confirm",
            post(duplicates::confirm_duplicate_pair::<R>),
        )
        .route(
            "/api/vocabulary/suggestions",
            get(vocabulary::list_vocabulary_suggestions::<R>),
        )
        .route(
            "/api/vocabulary/candidates",
            get(vocabulary::list_vocabulary_candidates::<R>),
        )
        .route(
            "/api/vocabulary/preflight",
            post(vocabulary::preflight_vocabulary_merge::<R>),
        )
        .route(
            "/api/vocabulary/merge",
            post(vocabulary::merge_vocabulary::<R>),
        )
        .route(
            "/api/vocabulary/reject",
            post(vocabulary::reject_vocabulary::<R>),
        )
        .route(
            "/api/saved-views",
            get(saved_views::list_saved_views::<R>).post(saved_views::create_saved_view::<R>),
        )
        .route(
            "/api/saved-views/{saved_view_id}",
            get(saved_views::get_saved_view::<R>)
                .put(saved_views::update_saved_view::<R>)
                .delete(saved_views::delete_saved_view::<R>),
        )
        .route(
            "/api/work-baskets",
            get(work_baskets::list_work_baskets::<R>),
        )
        .route(
            "/api/work-baskets/{basket_id}",
            get(work_baskets::get_work_basket::<R>),
        )
        .route(
            "/api/work-baskets/{basket_id}/collections",
            post(work_baskets::add_work_basket_collections::<R>)
                .delete(work_baskets::clear_work_basket::<R>),
        )
        .route(
            "/api/work-baskets/{basket_id}/collections/{collection_id}",
            axum::routing::delete(work_baskets::remove_work_basket_collection::<R>),
        )
        .route("/api/collections", get(collections::list_collections::<R>))
        .route(
            "/api/review-queue",
            get(collections::list_review_queue::<R>),
        )
        .route(
            "/api/collections/{collection_id}/locate",
            get(collections::locate_collection::<R>),
        )
        .route(
            "/api/collections/{collection_id}",
            get(collections::get_collection::<R>),
        )
        .route(
            "/api/collections/{collection_id}/open",
            post(collections::open_collection::<R>),
        )
        .route(
            "/api/collections/{collection_id}/read",
            post(collections::read_collection::<R>),
        )
        .route(
            "/api/collections/{collection_id}/thumbnail",
            get(thumbnails::get_thumbnail::<R>),
        )
        .route(
            "/api/collections/{collection_id}/thumbnail/rebuild",
            post(thumbnails::rebuild_thumbnail::<R>),
        )
        .route(
            "/api/collections/{collection_id}/cover-candidates",
            get(covers::get_cover_candidates::<R>),
        )
        .route(
            "/api/collections/{collection_id}/cover-candidates/preview",
            get(covers::get_cover_candidate_preview::<R>),
        )
        .route(
            "/api/collections/{collection_id}/cover-selection",
            put(covers::put_cover_selection::<R>).delete(covers::delete_cover_selection::<R>),
        )
        .route(
            "/api/thumbnails/rebuild",
            post(thumbnails::rebuild_all_thumbnails::<R>),
        )
        .route(
            "/api/thumbnail-cache-jobs",
            post(thumbnails::start_thumbnail_cache_job::<R>),
        )
        .route(
            "/api/thumbnail-cache-jobs/preflight",
            post(thumbnails::preflight_thumbnail_cache_job::<R>),
        )
        .route(
            "/api/thumbnail-cache-jobs/current",
            get(thumbnails::get_current_thumbnail_cache_job::<R>),
        )
        .route(
            "/api/thumbnail-cache-jobs/current/failures",
            get(thumbnails::get_thumbnail_cache_failures::<R>),
        )
        .route(
            "/api/thumbnail-cache-jobs/current/retry-failures",
            post(thumbnails::retry_thumbnail_cache_failures::<R>),
        )
        .route(
            "/api/collections/{collection_id}/metadata",
            get(metadata::get_metadata_history::<R>),
        )
        .route(
            "/api/collections/{collection_id}/metadata/{field}",
            put(metadata::set_manual_metadata::<R>).delete(metadata::clear_manual_metadata::<R>),
        )
        .route(
            "/api/collections/{collection_id}/metadata/{field}/assertions/{assertion_id}",
            patch(metadata::decide_metadata_assertion::<R>),
        )
        .route(
            "/api/collections/{collection_id}/tags",
            post(metadata::add_collection_tag::<R>).delete(metadata::remove_collection_tag::<R>),
        )
        .route(
            "/api/batch/tags",
            post(metadata::batch_add_collection_tag::<R>),
        )
        .route(
            "/api/batch/metadata/{field}",
            put(metadata::batch_set_manual_metadata::<R>),
        )
        .route(
            "/api/collections/{collection_id}/external-search-jobs",
            post(external_search::enqueue_external_search::<R>),
        )
        .route(
            "/api/external-search-jobs/activity",
            get(external_search::get_external_search_activity::<R>),
        )
        .route(
            "/api/external-search-jobs/{job_id}",
            get(external_search::get_external_search_job::<R>),
        )
        .route(
            "/api/external-search-jobs/{job_id}/acknowledge",
            post(external_search::acknowledge_external_search_job::<R>),
        )
        .route(
            "/api/external-search-batches/preflight",
            post(external_search::preflight_external_search_batch::<R>),
        )
        .route(
            "/api/external-search-batches",
            post(external_search::create_external_search_batch::<R>),
        )
        .route(
            "/api/external-search-batches/{batch_id}",
            get(external_search::get_external_search_batch::<R>),
        )
        .route(
            "/api/external-search-batches/{batch_id}/retry",
            post(external_search::retry_external_search_batch::<R>),
        )
        .route(
            "/api/tombstone-candidates",
            get(consolidation::list_tombstone_candidates::<R>),
        )
        .route(
            "/api/tombstone-candidates/{tombstone_id}/{candidate_id}",
            patch(consolidation::decide_tombstone_candidate::<R>),
        )
        .route(
            "/api/tombstone-candidates/{tombstone_id}/{candidate_id}/preflight",
            get(consolidation::consolidation_preflight::<R>),
        )
        .route(
            "/api/tombstone-candidates/{tombstone_id}/{candidate_id}/consolidate",
            post(consolidation::consolidate_tombstone_candidate::<R>),
        )
        .route(
            "/api/library-roots",
            get(roots::list_library_roots::<R>).post(roots::register_library_root::<R>),
        )
        .route(
            "/api/library-roots/{root_id}",
            patch(roots::update_library_root::<R>).delete(roots::deactivate_library_root::<R>),
        )
        .route(
            "/api/library-roots/{root_id}/activate",
            post(roots::reactivate_library_root::<R>),
        )
        .route(
            "/api/export-roots",
            get(exports::list_export_roots::<R>).post(exports::register_export_root::<R>),
        )
        .route(
            "/api/export-roots/{root_id}",
            patch(exports::update_export_root::<R>).delete(exports::deactivate_export_root::<R>),
        )
        .route(
            "/api/export-roots/{root_id}/activate",
            post(exports::reactivate_export_root::<R>),
        )
        .route(
            "/api/export-jobs/preflight",
            post(exports::preflight_export::<R>),
        )
        .route("/api/export-jobs", post(exports::create_export::<R>))
        .route(
            "/api/export-jobs/current",
            get(exports::get_current_export::<R>),
        )
        .route("/api/export-jobs/{job_id}", get(exports::get_export::<R>))
        .route(
            "/api/export-jobs/{job_id}/retry",
            post(exports::retry_export::<R>),
        )
        .route(
            "/api/export-jobs/{job_id}/open-location",
            post(exports::open_export_location::<R>),
        )
        .route(
            "/api/file-actions/move/preflight",
            post(files::preflight_move_collections::<R>),
        )
        .route("/api/file-actions/move", post(files::move_collections::<R>))
        .route(
            "/api/file-actions/rename/preflight",
            post(files::preflight_rename_collections::<R>),
        )
        .route(
            "/api/file-actions/rename",
            post(files::rename_collections::<R>),
        )
        .route(
            "/api/file-actions/delete",
            post(files::delete_collections::<R>),
        )
        .route("/api/scans", post(scan::start_scan::<R>))
        .route("/api/scans/preflight", post(scan::preflight_scan::<R>))
        .route("/api/scans/latest", get(scan::get_latest_scan::<R>))
        .route("/api/scans/{scan_run_id}", get(scan::get_scan::<R>))
        .fallback(error::not_found)
        .method_not_allowed_fallback(error::method_not_allowed)
        .layer(middleware::from_fn(guard::request_guard))
        .with_state(state)
}

pub(crate) struct HttpState<R> {
    pub(crate) application: Arc<Mutex<ApplicationService<R>>>,
    pub(crate) thumbnail_cache_jobs: Arc<Mutex<ThumbnailCacheJobs>>,
    pub(crate) instance_id: Option<String>,
}

impl<R> Clone for HttpState<R> {
    fn clone(&self) -> Self {
        Self {
            application: Arc::clone(&self.application),
            thumbnail_cache_jobs: Arc::clone(&self.thumbnail_cache_jobs),
            instance_id: self.instance_id.clone(),
        }
    }
}

#[derive(Default)]
pub(crate) struct ThumbnailCacheJobs {
    pub(crate) next_id: u64,
    pub(crate) current: Option<ThumbnailCacheJob>,
}

pub(crate) struct ThumbnailCacheJob {
    pub(crate) id: u64,
    pub(crate) root_ids: Vec<i64>,
    pub(crate) collection_ids: Vec<i64>,
    pub(crate) failed_collection_ids: Vec<i64>,
    pub(crate) initial_completed: usize,
    pub(crate) started_at: Instant,
}

const INTERACTIVE_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const INTERACTIVE_LOCK_RETRY: Duration = Duration::from_millis(10);

pub(crate) fn lock_interactive_application<R>(
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

async fn health<R>(State(state): State<HttpState<R>>) -> Json<HealthResponse>
where
    R: RecycleBin + Send + 'static,
{
    Json(HealthResponse {
        status: "ok",
        service: "doujin-http",
        api_version: 1,
        instance_id: state.instance_id,
    })
}
