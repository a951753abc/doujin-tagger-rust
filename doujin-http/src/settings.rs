//! Settings read/update endpoints.

use std::path::PathBuf;
use std::sync::TryLockError;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use doujin_app::ApplicationSettingsSnapshot;
use doujin_files::RecycleBin;
use serde::{Deserialize, Serialize};

use crate::HttpState;
use crate::error::ApiError;

#[derive(Debug, Serialize)]
pub(crate) struct SettingsResponse {
    viewer_path: String,
    thumb_size: String,
    thumb_quality: u8,
    saved_viewer_path: String,
    saved_thumb_size: String,
    saved_thumb_quality: u8,
    default_archive_root_id: Option<i64>,
    overrides: SettingsOverridesResponse,
    environment_overrides: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thumbnails_requeued: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SettingsOverridesResponse {
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
            default_archive_root_id: settings.default_archive_root_id,
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
pub(crate) struct SettingsUpdateRequest {
    viewer_path: String,
    thumb_size: String,
    thumb_quality: i64,
    #[serde(default)]
    default_archive_root_id: Option<i64>,
}

pub(crate) async fn get_settings<R>(
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

pub(crate) async fn update_settings<R>(
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
            .save_application_settings(
                reader_path,
                width,
                height,
                quality,
                payload.default_archive_root_id,
            )
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(SettingsResponse::from_snapshot(
        outcome.settings,
        Some(outcome.thumbnails_requeued),
    )))
}

pub(crate) fn parse_thumbnail_size(value: &str) -> Result<(u32, u32), ApiError> {
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
