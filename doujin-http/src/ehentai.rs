//! Loopback HTTP adapter for the E-Hentai/ExHentai external source.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use doujin_app::ApplicationError;
use doujin_files::RecycleBin;
use doujin_provider_ehentai::{
    CookieHeader, CookieStore, EhentaiSource, GallerySearchQuery, GalleryTorrent, SessionStatus,
    SourceError, SourceErrorKind, SourceGallery,
};
use doujin_storage::StorageError;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::{HttpState, SharedApplication, lock_interactive_application};

const COOKIE_ENV: &str = "DOUJIN_EXHENTAI_COOKIE";
const MAX_SEARCH_LENGTH: usize = 500;
const MAX_SEARCH_PAGE: u32 = 10_000;
const MAX_MAGNET_LENGTH: usize = 2_048;

pub trait MagnetLauncher: Send + Sync {
    fn open_magnet(&self, magnet_uri: &str) -> Result<(), String>;
}

struct SystemMagnetLauncher;

impl MagnetLauncher for SystemMagnetLauncher {
    fn open_magnet(&self, magnet_uri: &str) -> Result<(), String> {
        open::that_detached(magnet_uri)
            .map(|_| ())
            .map_err(|_| "無法啟動預設 BT client".to_owned())
    }
}

/// Shared HTTP runtime. Tests can inject a deterministic source and launcher;
/// production uses the same CookieStore as the metadata worker.
#[derive(Clone)]
pub struct EhentaiHttpServices {
    cookie_store: CookieStore,
    source: Arc<dyn EhentaiSource + Send + Sync>,
    launcher: Arc<dyn MagnetLauncher>,
    environment_override: bool,
    last_session: Arc<Mutex<Option<SessionStatus>>>,
}

impl EhentaiHttpServices {
    pub fn new(
        cookie_store: CookieStore,
        source: Arc<dyn EhentaiSource + Send + Sync>,
        launcher: Arc<dyn MagnetLauncher>,
        environment_override: bool,
    ) -> Self {
        Self {
            cookie_store,
            source,
            launcher,
            environment_override,
            last_session: Arc::new(Mutex::new(None)),
        }
    }

    pub fn cookie_store(&self) -> CookieStore {
        self.cookie_store.clone()
    }

    pub(crate) async fn production<R>(application: SharedApplication<R>) -> Result<Self, String>
    where
        R: RecycleBin + Send + 'static,
    {
        let environment_cookie = environment_cookie()?;
        let application_for_cookie = Arc::clone(&application);
        let catalog_cookie = tokio::task::spawn_blocking(move || {
            let application = application_for_cookie
                .lock()
                .map_err(|_| "application service lock 已失效".to_owned())?;
            match application.exhentai_cookie() {
                Ok(cookie) => Ok(cookie),
                Err(ApplicationError::Storage(StorageError::ExHentaiCookieUnavailable)) => Ok(None),
                Err(_) => Err("無法讀取已儲存的 ExHentai Cookie".to_owned()),
            }
        })
        .await
        .map_err(|_| "ExHentai Cookie 初始化工作中止".to_owned())??;
        let catalog_cookie = catalog_cookie
            .as_deref()
            .and_then(|value| CookieHeader::parse(value).ok());
        let environment_override = environment_cookie.is_some();
        let cookie_store = CookieStore::new(environment_cookie.or(catalog_cookie));
        let source_store = cookie_store.clone();
        let source = tokio::task::spawn_blocking(move || {
            doujin_provider_ehentai::ReqwestEhentaiSource::production(source_store)
                .map(|source| Arc::new(source) as Arc<dyn EhentaiSource + Send + Sync>)
                .map_err(|_| "無法建立 E-Hentai source client".to_owned())
        })
        .await
        .map_err(|_| "E-Hentai source 初始化工作中止".to_owned())??;
        Ok(Self::new(
            cookie_store,
            source,
            Arc::new(SystemMagnetLauncher),
            environment_override,
        ))
    }

    fn session(&self) -> Option<SessionStatus> {
        match self.last_session.lock() {
            Ok(status) => *status,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    fn set_session(&self, status: Option<SessionStatus>) {
        match self.last_session.lock() {
            Ok(mut current) => *current = status,
            Err(poisoned) => *poisoned.into_inner() = status,
        }
    }
}

fn environment_cookie() -> Result<Option<CookieHeader>, String> {
    let raw = match std::env::var(COOKIE_ENV) {
        Ok(value) if value.trim().is_empty() => return Ok(None),
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("DOUJIN_EXHENTAI_COOKIE 不是有效的 Unicode".to_owned());
        }
    };
    parse_environment_cookie_value(raw.trim())
}

fn parse_environment_cookie_value(raw: &str) -> Result<Option<CookieHeader>, String> {
    CookieHeader::parse(raw)
        .map(Some)
        .map_err(|_| "DOUJIN_EXHENTAI_COOKIE 格式無效".to_owned())
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionResponse {
    configured: bool,
    environment_override: bool,
    updated_at: Option<String>,
    session: Option<&'static str>,
}

impl SessionResponse {
    fn from_status(
        configured: bool,
        environment_override: bool,
        updated_at: Option<String>,
        session: Option<SessionStatus>,
    ) -> Self {
        Self {
            configured,
            environment_override,
            updated_at,
            session: session.map(SessionStatus::as_str),
        }
    }
}

pub(crate) async fn get_session<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<SessionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let services = state.ehentai.clone();
    session_response(state.application, services)
        .await
        .map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutSessionRequest {
    cookie: String,
}

pub(crate) async fn put_session<R>(
    State(state): State<HttpState<R>>,
    Json(request): Json<PutSessionRequest>,
) -> Result<Json<SessionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let parsed = CookieHeader::parse(request.cookie.trim()).map_err(|_| {
        ApiError::bad_request(
            "invalid_cookie",
            "Cookie header 格式無效；請重新貼上完整 Cookie",
        )
    })?;
    let application = Arc::clone(&state.application);
    let raw = request.cookie;
    let status = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&application)?;
        application
            .save_exhentai_cookie(raw.trim())
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    if !state.ehentai.environment_override {
        state.ehentai.cookie_store.set(Some(parsed));
        state.ehentai.set_session(None);
    }
    Ok(Json(SessionResponse::from_status(
        true,
        state.ehentai.environment_override,
        status.updated_at,
        state.ehentai.session(),
    )))
}

pub(crate) async fn delete_session<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<SessionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let application = Arc::clone(&state.application);
    tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&application)?;
        application
            .clear_exhentai_cookie()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    if !state.ehentai.environment_override {
        state.ehentai.cookie_store.set(None);
        state
            .ehentai
            .set_session(Some(SessionStatus::NotConfigured));
    }
    Ok(Json(SessionResponse::from_status(
        state.ehentai.environment_override,
        state.ehentai.environment_override,
        None,
        state.ehentai.session(),
    )))
}

pub(crate) async fn test_session<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<SessionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let source = Arc::clone(&state.ehentai.source);
    let status = tokio::task::spawn_blocking(move || source.validate_session())
        .await
        .map_err(|_| ApiError::internal())?;
    state.ehentai.set_session(Some(status));
    let response = session_response(state.application, state.ehentai).await?;
    Ok(Json(response))
}

async fn session_response<R>(
    application: SharedApplication<R>,
    services: EhentaiHttpServices,
) -> Result<SessionResponse, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let application_for_status = Arc::clone(&application);
    let status = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&application_for_status)?;
        let status = application
            .exhentai_session_status()
            .map_err(ApiError::from_application)?;
        if services.environment_override {
            return Ok(status);
        }
        match application.exhentai_cookie() {
            Ok(Some(cookie)) if CookieHeader::parse(&cookie).is_err() => Err(ApiError::conflict(
                "invalid_cookie",
                "已儲存的 Cookie 格式無效，請重新設定 Cookie",
            )),
            Ok(_) => Ok(status),
            Err(ApplicationError::Storage(StorageError::ExHentaiCookieUnavailable)) => {
                Err(ApiError::conflict(
                    "invalid_cookie",
                    "已儲存的 Cookie 無法解密，請清除後重新設定 Cookie",
                ))
            }
            Err(error) => Err(ApiError::from_application(error)),
        }
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(SessionResponse::from_status(
        services.environment_override || status.configured,
        services.environment_override,
        status.updated_at,
        services.session(),
    ))
}

#[derive(Debug, Deserialize)]
pub(crate) struct SearchParams {
    q: String,
    #[serde(default)]
    page: u32,
    cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchResponse {
    source: &'static str,
    page: u32,
    has_next: bool,
    next_cursor: Option<String>,
    previous_cursor: Option<String>,
    items: Vec<GalleryResponse>,
}

fn valid_search_cursor(cursor: &str) -> bool {
    let digits = cursor.strip_prefix("prev:").unwrap_or(cursor);
    (1..=20).contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) async fn search<R>(
    State(state): State<HttpState<R>>,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let query = params.q.trim();
    if query.is_empty()
        || query.chars().count() > MAX_SEARCH_LENGTH
        || params.page > MAX_SEARCH_PAGE
        || params
            .cursor
            .as_deref()
            .is_some_and(|cursor| !valid_search_cursor(cursor))
    {
        return Err(ApiError::bad_request(
            "invalid_search_query",
            "搜尋文字不可為空、長度不可超過 500 字，page 與 cursor 必須在允許範圍內",
        ));
    }
    let source = Arc::clone(&state.ehentai.source);
    let request = GallerySearchQuery {
        query: query.to_owned(),
        page: params.page,
        cursor: params.cursor,
    };
    let result = tokio::task::spawn_blocking(move || source.search(&request))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(source_error)?;
    Ok(Json(SearchResponse {
        source: result.source.as_str(),
        page: result.page,
        has_next: result.has_next,
        next_cursor: result.next_cursor,
        previous_cursor: result.previous_cursor,
        items: result
            .galleries
            .into_iter()
            .map(GalleryResponse::from)
            .collect(),
    }))
}

#[derive(Debug, Serialize)]
pub(crate) struct GalleryEnvelope {
    source: &'static str,
    gallery: GalleryResponse,
}

pub(crate) async fn gallery<R>(
    State(state): State<HttpState<R>>,
    Path((gid, token)): Path<(u64, String)>,
) -> Result<Json<GalleryEnvelope>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let source = Arc::clone(&state.ehentai.source);
    let result = tokio::task::spawn_blocking(move || source.gallery(gid, &token))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(source_error)?;
    Ok(Json(GalleryEnvelope {
        source: result.source.as_str(),
        gallery: result.into(),
    }))
}

#[derive(Debug, Serialize)]
pub(crate) struct GalleryResponse {
    source: &'static str,
    gid: u64,
    token: String,
    title: String,
    title_jpn: Option<String>,
    category: String,
    thumb: Option<String>,
    uploader: Option<String>,
    posted_at: Option<String>,
    rating: Option<f64>,
    tags: Vec<String>,
    pages: Option<u32>,
}

impl From<SourceGallery> for GalleryResponse {
    fn from(gallery: SourceGallery) -> Self {
        Self {
            source: gallery.source.as_str(),
            gid: gallery.gid,
            token: gallery.token,
            title: gallery.title,
            title_jpn: gallery.title_jpn,
            category: gallery.category,
            thumb: gallery.thumb,
            uploader: gallery.uploader,
            posted_at: gallery.posted,
            rating: gallery.rating,
            tags: gallery.tags,
            pages: gallery.pages,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct TorrentListResponse {
    source: &'static str,
    items: Vec<TorrentResponse>,
}

pub(crate) async fn torrents<R>(
    State(state): State<HttpState<R>>,
    Path((gid, token)): Path<(u64, String)>,
) -> Result<Json<TorrentListResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let source = Arc::clone(&state.ehentai.source);
    let result = tokio::task::spawn_blocking(move || source.torrents(gid, &token))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(source_error)?;
    Ok(Json(TorrentListResponse {
        source: result.source.as_str(),
        items: result.torrents.into_iter().map(Into::into).collect(),
    }))
}

#[derive(Debug, Serialize)]
pub(crate) struct TorrentResponse {
    name: String,
    posted_at: String,
    size: String,
    seeds: u32,
    peers: u32,
    downloads: u32,
    outdated: bool,
    torrent_url: String,
    magnet_url: Option<String>,
}

impl From<GalleryTorrent> for TorrentResponse {
    fn from(torrent: GalleryTorrent) -> Self {
        Self {
            name: torrent.name,
            posted_at: torrent.posted_at,
            size: torrent.size,
            seeds: torrent.seeds,
            peers: torrent.peers,
            downloads: torrent.downloads,
            outdated: torrent.outdated,
            torrent_url: torrent.torrent_url,
            magnet_url: torrent.magnet_url,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DownloadRequest {
    url: String,
    name: String,
}

pub(crate) async fn download_torrent<R>(
    State(state): State<HttpState<R>>,
    Json(request): Json<DownloadRequest>,
) -> Result<Response, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let source = Arc::clone(&state.ehentai.source);
    let url = request.url;
    let download = tokio::task::spawn_blocking(move || source.download_torrent(&url))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(source_error)?;
    let filename = attachment_filename(&request.name);
    let mut response = Body::from(download.bytes).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-bittorrent"),
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .map_err(|_| ApiError::internal())?,
    );
    Ok(response)
}

fn attachment_filename(name: &str) -> String {
    let mut safe = name
        .trim()
        .trim_end_matches(|character: char| character.is_whitespace())
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric()
                || matches!(character, ' ' | '-' | '_' | '.' | '(' | ')' | '[' | ']')
            {
                character
            } else {
                '_'
            }
        })
        .take(120)
        .collect::<String>();
    safe = safe.trim_matches([' ', '.']).to_owned();
    if safe.is_empty() {
        safe.push_str("gallery");
    }
    if !safe.to_ascii_lowercase().ends_with(".torrent") {
        safe.push_str(".torrent");
    }
    safe
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MagnetRequest {
    magnet_uri: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct MagnetResponse {
    opened: bool,
}

pub(crate) async fn open_magnet<R>(
    State(state): State<HttpState<R>>,
    Json(request): Json<MagnetRequest>,
) -> Result<Json<MagnetResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    validate_magnet(&request.magnet_uri)?;
    let launcher = Arc::clone(&state.ehentai.launcher);
    let magnet_uri = request.magnet_uri;
    tokio::task::spawn_blocking(move || launcher.open_magnet(&magnet_uri))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "external_launch_failed",
                "無法啟動預設 BT client",
            )
        })?;
    Ok(Json(MagnetResponse { opened: true }))
}

fn validate_magnet(value: &str) -> Result<(), ApiError> {
    if value.len() > MAX_MAGNET_LENGTH
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
        || value.to_ascii_lowercase().contains("%0d")
        || value.to_ascii_lowercase().contains("%0a")
        || !value.starts_with("magnet:?")
    {
        return Err(invalid_magnet());
    }
    let hash = value[8..]
        .split('&')
        .find_map(|part| part.strip_prefix("xt=urn:btih:"))
        .ok_or_else(invalid_magnet)?;
    let valid_hash = (hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || (hash.len() == 32
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'2'..=b'7')));
    if valid_hash {
        Ok(())
    } else {
        Err(invalid_magnet())
    }
}

fn invalid_magnet() -> ApiError {
    ApiError::bad_request(
        "invalid_magnet",
        "magnet URI 必須包含 40 hex 或 32 base32 的 BTIH hash",
    )
}

fn source_error(error: SourceError) -> ApiError {
    match error.kind {
        SourceErrorKind::InvalidCookie => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_cookie",
            "ExHentai Cookie 已失效，請重新設定 Cookie",
        ),
        SourceErrorKind::ExhentaiUnavailable => ApiError::unavailable(
            "exhentai_unavailable",
            "ExHentai 暫時無法使用，且公開 E-Hentai fallback 沒有可用結果",
        ),
        SourceErrorKind::RateLimited => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "E-Hentai 暫時限制 request，請稍後再試",
        ),
        SourceErrorKind::NetworkError => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "network_error",
            "無法連線至 E-Hentai",
        ),
        SourceErrorKind::ParseError => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "parse_error",
            "E-Hentai response 格式無法解讀",
        ),
        SourceErrorKind::GalleryNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "gallery_not_found",
            "找不到指定的 E-Hentai gallery",
        ),
        SourceErrorKind::TorrentNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "torrent_not_found",
            "gallery 沒有可用的 torrent",
        ),
        SourceErrorKind::UnsafeTorrentUrl => ApiError::bad_request(
            "unsafe_torrent_url",
            "torrent URL 不在允許的 E-Hentai HTTPS 範圍",
        ),
        SourceErrorKind::TorrentTooLarge => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "torrent_too_large",
            "torrent 超過允許的大小",
        ),
        SourceErrorKind::InvalidTorrent => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "invalid_torrent",
            "下載結果不是有效的 torrent",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_environment_cookie_error_is_redacted() {
        let secret = "ipb_pass_hash=do-not-leak\r\nX-Evil: yes";
        let error = parse_environment_cookie_value(secret).expect_err("invalid environment");
        assert!(!error.contains("do-not-leak"));
        assert!(!format!("{error:?}").contains("do-not-leak"));
    }

    #[test]
    fn attachment_filename_and_magnet_validation_are_bounded() {
        let filename = attachment_filename("..\\bad:\r\n名稱/part");
        assert!(filename.ends_with(".torrent"));
        assert!(!filename.contains(['\r', '\n', '\\', '/', ':', '名']));
        assert!(filename.len() <= 128);

        let hex = format!("magnet:?xt=urn:btih:{}", "a".repeat(40));
        let base32 = format!("magnet:?xt=urn:btih:{}", "A".repeat(32));
        assert!(validate_magnet(&hex).is_ok());
        assert!(validate_magnet(&base32).is_ok());
        assert!(validate_magnet("https://example.invalid/file.torrent").is_err());
        assert!(validate_magnet("magnet:?xt=urn:btih:short").is_err());
        assert!(validate_magnet(&format!("{hex}%0d%0aX-Evil:yes")).is_err());
    }

    #[test]
    fn every_source_error_maps_to_a_stable_redacted_http_error() {
        let cases = [
            (SourceErrorKind::InvalidCookie, "invalid_cookie"),
            (SourceErrorKind::ExhentaiUnavailable, "exhentai_unavailable"),
            (SourceErrorKind::RateLimited, "rate_limited"),
            (SourceErrorKind::NetworkError, "network_error"),
            (SourceErrorKind::ParseError, "parse_error"),
            (SourceErrorKind::GalleryNotFound, "gallery_not_found"),
            (SourceErrorKind::TorrentNotFound, "torrent_not_found"),
            (SourceErrorKind::UnsafeTorrentUrl, "unsafe_torrent_url"),
            (SourceErrorKind::TorrentTooLarge, "torrent_too_large"),
            (SourceErrorKind::InvalidTorrent, "invalid_torrent"),
        ];
        for (kind, code) in cases {
            let error = source_error(SourceError {
                kind,
                message: "https://example.invalid/?secret=do-not-leak".to_owned(),
            });
            assert_eq!(code, error.code);
            assert!(!error.message.contains("do-not-leak"));
            assert!(!error.message.contains("example.invalid"));
        }
    }
}
