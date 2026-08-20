//! API error envelope and fallback handlers.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use doujin_app::ApplicationError;
use doujin_files::LaunchError;
use doujin_storage::StorageError;
use serde::Serialize;

pub(crate) async fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "route_not_found",
        "找不到指定的 API route",
    )
}

pub(crate) async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "此 API route 不支援指定的 HTTP method",
    )
}

#[derive(Debug, Serialize)]
pub(crate) struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub(crate) struct ErrorBody {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    merged_into_collection_id: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
    merged_into_collection_id: Option<i64>,
}

impl ApiError {
    pub(crate) fn bad_request(code: &'static str, message: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub(crate) fn forbidden(code: &'static str, message: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    pub(crate) fn conflict(code: &'static str, message: &str) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub(crate) fn unavailable(code: &'static str, message: &str) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, code, message)
    }

    pub(crate) fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "伺服器內部錯誤",
        )
    }

    pub(crate) fn merged(survivor_id: i64) -> Self {
        Self {
            status: StatusCode::GONE,
            code: "collection_merged",
            message: format!("收藏已合併至 collection {survivor_id}"),
            merged_into_collection_id: Some(survivor_id),
        }
    }

    pub(crate) fn new(status: StatusCode, code: &'static str, message: &str) -> Self {
        Self {
            status,
            code,
            message: message.to_owned(),
            merged_into_collection_id: None,
        }
    }

    pub(crate) fn from_application(error: ApplicationError) -> Self {
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
            | ApplicationError::ExportIo(_)
            | ApplicationError::Json(_) => Self::internal(),
        }
    }

    pub(crate) fn from_cover_application(error: ApplicationError) -> Self {
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

    pub(crate) fn from_launch(error: LaunchError) -> Self {
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

    pub(crate) fn from_storage(error: StorageError) -> Self {
        match error {
            StorageError::LibraryRootNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "library_root_not_found",
                "找不到指定的 library root",
            ),
            StorageError::InvalidLibraryRoot(reason) => {
                Self::bad_request("invalid_library_root", &reason)
            }
            StorageError::ExportRootNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "export_root_not_found",
                "找不到指定的 export root",
            ),
            StorageError::InvalidExportRoot(reason) => {
                Self::bad_request("invalid_export_root", &reason)
            }
            StorageError::ExportJobNotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "export_job_not_found",
                "找不到指定的 export job",
            ),
            StorageError::InvalidExportJob(reason) => {
                Self::bad_request("invalid_export_job", &reason)
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
            StorageError::InvalidShelfConfiguration(reason) => {
                Self::bad_request("invalid_shelf_configuration", &reason)
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
