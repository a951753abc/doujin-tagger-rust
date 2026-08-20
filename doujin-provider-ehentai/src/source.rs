use std::error::Error;
use std::fmt;
use std::io::Read;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, COOKIE, LOCATION};
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;

use crate::cookie::{CookieHeader, CookieStore};
use crate::{GalleryIdentity, gallery_identities_from_search};

const EHENTAI_BASE: &str = "https://e-hentai.org";
const EXHENTAI_BASE: &str = "https://exhentai.org";
const GDATA_URL: &str = "https://api.e-hentai.org/api.php";
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_TORRENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TORRENT_REDIRECTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceSite {
    Ehentai,
    Exhentai,
}

impl SourceSite {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ehentai => "ehentai",
            Self::Exhentai => "exhentai",
        }
    }

    fn base_url(self) -> &'static str {
        match self {
            Self::Ehentai => EHENTAI_BASE,
            Self::Exhentai => EXHENTAI_BASE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Exhentai,
    EhentaiOnly,
    NotConfigured,
    InvalidCookie,
    ExhentaiUnavailable,
    RateLimited,
    NetworkError,
    ParseError,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exhentai => "exhentai",
            Self::EhentaiOnly => "ehentai_only",
            Self::NotConfigured => "not_configured",
            Self::InvalidCookie => "invalid_cookie",
            Self::ExhentaiUnavailable => "exhentai_unavailable",
            Self::RateLimited => "rate_limited",
            Self::NetworkError => "network_error",
            Self::ParseError => "parse_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceErrorKind {
    InvalidCookie,
    ExhentaiUnavailable,
    RateLimited,
    NetworkError,
    ParseError,
    GalleryNotFound,
    TorrentNotFound,
    UnsafeTorrentUrl,
    TorrentTooLarge,
    InvalidTorrent,
}

impl SourceErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidCookie => "invalid_cookie",
            Self::ExhentaiUnavailable => "exhentai_unavailable",
            Self::RateLimited => "rate_limited",
            Self::NetworkError => "network_error",
            Self::ParseError => "parse_error",
            Self::GalleryNotFound => "gallery_not_found",
            Self::TorrentNotFound => "torrent_not_found",
            Self::UnsafeTorrentUrl => "unsafe_torrent_url",
            Self::TorrentTooLarge => "torrent_too_large",
            Self::InvalidTorrent => "invalid_torrent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError {
    pub kind: SourceErrorKind,
    pub message: String,
}

impl SourceError {
    fn new(kind: SourceErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl Error for SourceError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GallerySearchQuery {
    pub query: String,
    /// Zero-based logical page maintained by the local UI.
    pub page: u32,
    /// Opaque cursor returned by the E-Hentai pager.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceGallery {
    pub source: SourceSite,
    pub gid: u64,
    pub token: String,
    pub title: String,
    pub title_jpn: Option<String>,
    pub category: String,
    pub thumb: Option<String>,
    pub uploader: Option<String>,
    pub posted: Option<String>,
    pub rating: Option<f64>,
    pub tags: Vec<String>,
    pub pages: Option<u32>,
}

pub type GalleryDetail = SourceGallery;

#[derive(Debug, Clone, PartialEq)]
pub struct GallerySearchPage {
    pub source: SourceSite,
    pub page: u32,
    pub has_next: bool,
    pub next_cursor: Option<String>,
    pub previous_cursor: Option<String>,
    pub galleries: Vec<SourceGallery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GalleryTorrent {
    pub name: String,
    pub posted_at: String,
    pub size: String,
    pub seeds: u32,
    pub peers: u32,
    pub downloads: u32,
    pub outdated: bool,
    pub torrent_url: String,
    pub magnet_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GalleryTorrentList {
    pub source: SourceSite,
    pub torrents: Vec<GalleryTorrent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentDownload {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

pub trait EhentaiSource {
    fn validate_session(&self) -> SessionStatus;
    fn search(&self, query: &GallerySearchQuery) -> Result<GallerySearchPage, SourceError>;
    fn gallery(&self, gid: u64, token: &str) -> Result<GalleryDetail, SourceError>;
    fn torrents(&self, gid: u64, token: &str) -> Result<GalleryTorrentList, SourceError>;
    fn download_torrent(&self, url: &str) -> Result<TorrentDownload, SourceError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportResponse {
    pub status: u16,
    pub final_url: String,
    pub content_type: Option<String>,
    pub location: Option<String>,
    pub body: Vec<u8>,
    pub exceeded_limit: bool,
}

pub trait SourceTransport: Send + Sync {
    fn get(
        &self,
        url: &str,
        cookie: Option<&CookieHeader>,
        max_bytes: usize,
    ) -> Result<TransportResponse, SourceError>;

    fn post_json(
        &self,
        url: &str,
        cookie: Option<&CookieHeader>,
        body: &str,
        max_bytes: usize,
    ) -> Result<TransportResponse, SourceError>;
}

pub struct ReqwestSourceTransport {
    client: reqwest::blocking::Client,
}

impl ReqwestSourceTransport {
    pub fn new() -> Result<Self, SourceError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("doujin-tagger/0.1 (local E-Hentai source)")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                SourceError::new(
                    SourceErrorKind::NetworkError,
                    "無法建立 E-Hentai HTTP client",
                )
            })?;
        Ok(Self { client })
    }

    fn send(
        &self,
        mut request: reqwest::blocking::RequestBuilder,
        cookie: Option<&CookieHeader>,
        max_bytes: usize,
    ) -> Result<TransportResponse, SourceError> {
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie.request_header_value());
        }
        let response = request.send().map_err(|_| {
            SourceError::new(SourceErrorKind::NetworkError, "E-Hentai request 失敗")
        })?;
        let status = response.status().as_u16();
        let final_url = response.url().to_string();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        response
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut body)
            .map_err(|_| {
                SourceError::new(SourceErrorKind::NetworkError, "無法讀取 E-Hentai response")
            })?;
        let exceeded_limit = body.len() > max_bytes;
        if exceeded_limit {
            body.truncate(max_bytes);
        }
        Ok(TransportResponse {
            status,
            final_url,
            content_type,
            location,
            body,
            exceeded_limit,
        })
    }
}

impl SourceTransport for ReqwestSourceTransport {
    fn get(
        &self,
        url: &str,
        cookie: Option<&CookieHeader>,
        max_bytes: usize,
    ) -> Result<TransportResponse, SourceError> {
        self.send(self.client.get(url), cookie, max_bytes)
    }

    fn post_json(
        &self,
        url: &str,
        cookie: Option<&CookieHeader>,
        body: &str,
        max_bytes: usize,
    ) -> Result<TransportResponse, SourceError> {
        self.send(
            self.client
                .post(url)
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_owned()),
            cookie,
            max_bytes,
        )
    }
}

pub struct ReqwestEhentaiSource<T = ReqwestSourceTransport> {
    transport: T,
    cookies: CookieStore,
    max_torrent_bytes: usize,
}

impl ReqwestEhentaiSource<ReqwestSourceTransport> {
    pub fn production(cookies: CookieStore) -> Result<Self, SourceError> {
        Ok(Self::with_transport(
            cookies,
            ReqwestSourceTransport::new()?,
        ))
    }
}

impl<T> ReqwestEhentaiSource<T> {
    pub fn with_transport(cookies: CookieStore, transport: T) -> Self {
        Self {
            transport,
            cookies,
            max_torrent_bytes: DEFAULT_MAX_TORRENT_BYTES,
        }
    }

    pub fn cookie_store(&self) -> CookieStore {
        self.cookies.clone()
    }

    pub fn with_max_torrent_bytes(mut self, max_bytes: usize) -> Self {
        self.max_torrent_bytes = max_bytes;
        self
    }
}

impl<T: SourceTransport> EhentaiSource for ReqwestEhentaiSource<T> {
    fn validate_session(&self) -> SessionStatus {
        let Some(cookie) = self.cookies.snapshot() else {
            return SessionStatus::NotConfigured;
        };
        if !cookie.contains("ipb_member_id") || !cookie.contains("ipb_pass_hash") {
            return match self.transport.get(
                "https://e-hentai.org/?nw=1",
                Some(&cookie),
                MAX_PAGE_BYTES,
            ) {
                Err(error) => session_status_for_error(error.kind),
                Ok(response) => classify_public_session_response(&response),
            };
        }
        match self
            .transport
            .get(EXHENTAI_BASE, Some(&cookie), MAX_PAGE_BYTES)
        {
            Err(error) => session_status_for_error(error.kind),
            Ok(response) => classify_session_response(&response),
        }
    }

    fn search(&self, query: &GallerySearchQuery) -> Result<GallerySearchPage, SourceError> {
        let cookie = self.cookies.snapshot();
        if let Some(cookie) = cookie.as_ref() {
            match self.search_once(SourceSite::Exhentai, Some(cookie), query) {
                Ok(result) => return Ok(result),
                Err(primary) if should_public_fallback(primary.kind) => {
                    return match self.search_once(SourceSite::Ehentai, None, query) {
                        Ok(page) if !page.galleries.is_empty() => Ok(page),
                        Ok(_) | Err(_) => Err(primary),
                    };
                }
                Err(error) => return Err(error),
            }
        }
        self.search_once(SourceSite::Ehentai, None, query)
    }

    fn gallery(&self, gid: u64, token: &str) -> Result<GalleryDetail, SourceError> {
        let identity = checked_identity(gid, token)?;
        let cookie = self.cookies.snapshot();
        if let Some(cookie) = cookie.as_ref() {
            match self.gallery_once(SourceSite::Exhentai, Some(cookie), &identity) {
                Ok(result) => return Ok(result),
                Err(primary) if should_public_fallback(primary.kind) => {
                    return self
                        .gallery_once(SourceSite::Ehentai, None, &identity)
                        .or(Err(primary));
                }
                Err(error) => return Err(error),
            }
        }
        self.gallery_once(SourceSite::Ehentai, None, &identity)
    }

    fn torrents(&self, gid: u64, token: &str) -> Result<GalleryTorrentList, SourceError> {
        let identity = checked_identity(gid, token)?;
        let cookie = self.cookies.snapshot();
        if let Some(cookie) = cookie.as_ref() {
            match self.torrents_once(SourceSite::Exhentai, Some(cookie), &identity) {
                Ok(result) => return Ok(result),
                Err(primary) if should_public_fallback(primary.kind) => {
                    return self
                        .torrents_once(SourceSite::Ehentai, None, &identity)
                        .or(Err(primary));
                }
                Err(error) => return Err(error),
            }
        }
        self.torrents_once(SourceSite::Ehentai, None, &identity)
    }

    fn download_torrent(&self, url: &str) -> Result<TorrentDownload, SourceError> {
        let mut current = reqwest::Url::parse(url).map_err(|_| unsafe_torrent_url())?;
        let cookie = self.cookies.snapshot();
        for redirects in 0..=MAX_TORRENT_REDIRECTS {
            validate_torrent_url(&current)?;
            let request_cookie = match current.host_str() {
                Some("e-hentai.org" | "exhentai.org") => cookie.as_ref(),
                _ => None,
            };
            let response =
                self.transport
                    .get(current.as_str(), request_cookie, self.max_torrent_bytes)?;
            if response.exceeded_limit {
                return Err(SourceError::new(
                    SourceErrorKind::TorrentTooLarge,
                    "torrent 超過允許的大小",
                ));
            }
            if (300..400).contains(&response.status) {
                if redirects == MAX_TORRENT_REDIRECTS {
                    return Err(SourceError::new(
                        SourceErrorKind::UnsafeTorrentUrl,
                        "torrent redirect 次數過多",
                    ));
                }
                let location = response.location.as_deref().ok_or_else(|| {
                    SourceError::new(
                        SourceErrorKind::InvalidTorrent,
                        "torrent redirect 缺少 Location",
                    )
                })?;
                current = current.join(location).map_err(|_| unsafe_torrent_url())?;
                continue;
            }
            if matches!(response.status, 404 | 410) {
                return Err(SourceError::new(
                    SourceErrorKind::TorrentNotFound,
                    "torrent 已不存在或下載網址已失效",
                ));
            }
            classify_http_status(response.status, false)?;
            if is_html_content_type(response.content_type.as_deref())
                || looks_like_html(&response.body)
                || !is_valid_torrent(&response.body)
            {
                return Err(SourceError::new(
                    SourceErrorKind::InvalidTorrent,
                    "下載結果不是有效的 torrent",
                ));
            }
            return Ok(TorrentDownload {
                bytes: response.body,
                content_type: response.content_type,
            });
        }
        unreachable!("bounded redirect loop")
    }
}

impl<T: SourceTransport> ReqwestEhentaiSource<T> {
    fn search_once(
        &self,
        site: SourceSite,
        cookie: Option<&CookieHeader>,
        query: &GallerySearchQuery,
    ) -> Result<GallerySearchPage, SourceError> {
        let mut url = reqwest::Url::parse(site.base_url()).expect("fixed base URL");
        url.query_pairs_mut()
            .append_pair("f_search", query.query.trim());
        if let Some(cursor) = query.cursor.as_deref() {
            if let Some(cursor) = cursor.strip_prefix("prev:") {
                url.query_pairs_mut().append_pair("prev", cursor);
            } else {
                url.query_pairs_mut().append_pair("next", cursor);
            }
        }
        let response = self.transport.get(url.as_str(), cookie, MAX_PAGE_BYTES)?;
        let body = page_body(&response, site == SourceSite::Exhentai)?;
        let identities = gallery_identities_from_search(body).map_err(|_| {
            SourceError::new(SourceErrorKind::ParseError, "無法解析 gallery 搜尋結果")
        })?;
        let next_cursor = search_next_cursor(body);
        let previous_cursor = search_previous_cursor(body);
        let galleries = if identities.is_empty() {
            Vec::new()
        } else {
            self.fetch_gdata(cookie, site, &identities)?
        };
        Ok(GallerySearchPage {
            source: site,
            page: query.page,
            has_next: next_cursor.is_some(),
            next_cursor,
            previous_cursor,
            galleries,
        })
    }

    fn gallery_once(
        &self,
        site: SourceSite,
        cookie: Option<&CookieHeader>,
        identity: &GalleryIdentity,
    ) -> Result<GalleryDetail, SourceError> {
        let url = format!("{}/g/{}/{}/", site.base_url(), identity.gid, identity.token);
        let response = self.transport.get(&url, cookie, MAX_PAGE_BYTES)?;
        page_body(&response, site == SourceSite::Exhentai)?;
        self.fetch_gdata(cookie, site, std::slice::from_ref(identity))?
            .into_iter()
            .find(|gallery| gallery.gid == identity.gid && gallery.token == identity.token)
            .ok_or_else(|| {
                SourceError::new(
                    SourceErrorKind::GalleryNotFound,
                    "gdata 沒有回傳指定 gallery",
                )
            })
    }

    fn torrents_once(
        &self,
        site: SourceSite,
        cookie: Option<&CookieHeader>,
        identity: &GalleryIdentity,
    ) -> Result<GalleryTorrentList, SourceError> {
        let mut url = reqwest::Url::parse(&format!("{}/gallerytorrents.php", site.base_url()))
            .expect("fixed torrent URL");
        url.query_pairs_mut()
            .append_pair("gid", &identity.gid.to_string())
            .append_pair("t", &identity.token);
        let response = self.transport.get(url.as_str(), cookie, MAX_PAGE_BYTES)?;
        let body = page_body(&response, site == SourceSite::Exhentai)?;
        let torrents = parse_torrent_list(body)?;
        if torrents.is_empty() {
            return Err(SourceError::new(
                SourceErrorKind::TorrentNotFound,
                "gallery 沒有可用的 torrent",
            ));
        }
        Ok(GalleryTorrentList {
            source: site,
            torrents,
        })
    }

    fn fetch_gdata(
        &self,
        cookie: Option<&CookieHeader>,
        site: SourceSite,
        identities: &[GalleryIdentity],
    ) -> Result<Vec<SourceGallery>, SourceError> {
        let body = serde_json::json!({
            "method": "gdata",
            "gidlist": identities.iter().map(|identity| {
                serde_json::json!([identity.gid, identity.token])
            }).collect::<Vec<_>>(),
            "namespace": 1,
        })
        .to_string();
        let response = self
            .transport
            .post_json(GDATA_URL, cookie, &body, MAX_PAGE_BYTES)?;
        classify_http_status(response.status, false)?;
        if response.exceeded_limit {
            return Err(SourceError::new(
                SourceErrorKind::ParseError,
                "gdata response 過大",
            ));
        }
        parse_source_gdata(&response.body, site, identities)
    }
}

fn checked_identity(gid: u64, token: &str) -> Result<GalleryIdentity, SourceError> {
    GalleryIdentity::parse(&format!("{gid}/{token}"))
        .ok_or_else(|| SourceError::new(SourceErrorKind::ParseError, "gallery gid/token 格式無效"))
}

fn session_status_for_error(kind: SourceErrorKind) -> SessionStatus {
    match kind {
        SourceErrorKind::InvalidCookie => SessionStatus::InvalidCookie,
        SourceErrorKind::ExhentaiUnavailable => SessionStatus::ExhentaiUnavailable,
        SourceErrorKind::RateLimited => SessionStatus::RateLimited,
        SourceErrorKind::NetworkError => SessionStatus::NetworkError,
        _ => SessionStatus::ParseError,
    }
}

fn classify_session_response(response: &TransportResponse) -> SessionStatus {
    if response.exceeded_limit {
        return SessionStatus::ParseError;
    }
    if response.status == 429 || looks_rate_limited(&response.body) {
        return SessionStatus::RateLimited;
    }
    if matches!(response.status, 401 | 403) {
        return SessionStatus::InvalidCookie;
    }
    if (500..600).contains(&response.status) {
        return SessionStatus::ExhentaiUnavailable;
    }
    if (300..400).contains(&response.status) {
        return SessionStatus::InvalidCookie;
    }
    if response.status != 200 {
        return SessionStatus::ParseError;
    }
    if is_sad_panda(&response.body) {
        return SessionStatus::ExhentaiUnavailable;
    }
    if looks_like_html(&response.body) {
        SessionStatus::Exhentai
    } else {
        SessionStatus::ParseError
    }
}

fn classify_public_session_response(response: &TransportResponse) -> SessionStatus {
    if response.exceeded_limit {
        return SessionStatus::ParseError;
    }
    if response.status == 429 || looks_rate_limited(&response.body) {
        return SessionStatus::RateLimited;
    }
    if matches!(response.status, 401 | 403) {
        return SessionStatus::InvalidCookie;
    }
    if response.status != 200 {
        return SessionStatus::NetworkError;
    }
    if looks_like_html(&response.body) {
        SessionStatus::EhentaiOnly
    } else {
        SessionStatus::ParseError
    }
}

fn page_body(response: &TransportResponse, exhentai: bool) -> Result<&str, SourceError> {
    if response.exceeded_limit {
        return Err(SourceError::new(
            SourceErrorKind::ParseError,
            "E-Hentai response 過大",
        ));
    }
    if response.status == 429 || looks_rate_limited(&response.body) {
        return Err(SourceError::new(
            SourceErrorKind::RateLimited,
            "E-Hentai 暫時限制 request",
        ));
    }
    if exhentai && matches!(response.status, 401 | 403) {
        return Err(SourceError::new(
            SourceErrorKind::InvalidCookie,
            "ExHentai Cookie 已失效或被拒絕",
        ));
    }
    if exhentai && (300..400).contains(&response.status) {
        return Err(SourceError::new(
            SourceErrorKind::InvalidCookie,
            "ExHentai 將 request 導向登入或公開站",
        ));
    }
    classify_http_status(response.status, exhentai)?;
    if exhentai && is_sad_panda(&response.body) {
        return Err(SourceError::new(
            SourceErrorKind::ExhentaiUnavailable,
            "ExHentai 回傳 Sad Panda",
        ));
    }
    if !looks_like_html(&response.body) {
        return Err(SourceError::new(
            SourceErrorKind::ParseError,
            "E-Hentai response 不是 HTML page",
        ));
    }
    std::str::from_utf8(&response.body).map_err(|_| {
        SourceError::new(
            SourceErrorKind::ParseError,
            "E-Hentai response 不是有效的 UTF-8",
        )
    })
}

fn classify_http_status(status: u16, exhentai: bool) -> Result<(), SourceError> {
    match status {
        200 => Ok(()),
        404 => Err(SourceError::new(
            SourceErrorKind::GalleryNotFound,
            "E-Hentai 回傳 HTTP 404",
        )),
        429 => Err(SourceError::new(
            SourceErrorKind::RateLimited,
            "E-Hentai 回傳 HTTP 429",
        )),
        500..=599 if exhentai => Err(SourceError::new(
            SourceErrorKind::ExhentaiUnavailable,
            "ExHentai 暫時無法使用",
        )),
        400..=499 if exhentai => Err(SourceError::new(
            SourceErrorKind::InvalidCookie,
            "ExHentai request 被拒絕",
        )),
        _ => Err(SourceError::new(
            SourceErrorKind::NetworkError,
            format!("E-Hentai 回傳 HTTP {status}"),
        )),
    }
}

fn should_public_fallback(kind: SourceErrorKind) -> bool {
    matches!(
        kind,
        SourceErrorKind::InvalidCookie | SourceErrorKind::ExhentaiUnavailable
    )
}

fn is_sad_panda(body: &[u8]) -> bool {
    let body = String::from_utf8_lossy(body).to_ascii_lowercase();
    body.contains("sad panda")
        || body.contains("sadpanda.jpg")
        || (body.contains("panda") && body.contains("exhentai"))
}

fn looks_rate_limited(body: &[u8]) -> bool {
    let body = String::from_utf8_lossy(body).to_ascii_lowercase();
    body.contains("temporarily banned") || body.contains("rate limit")
}

fn looks_like_html(body: &[u8]) -> bool {
    let prefix = String::from_utf8_lossy(&body[..body.len().min(512)]).to_ascii_lowercase();
    prefix.contains("<!doctype html") || prefix.contains("<html") || prefix.contains("<body")
}

fn is_html_content_type(content_type: Option<&str>) -> bool {
    content_type
        .map(|value| value.to_ascii_lowercase().contains("text/html"))
        .unwrap_or(false)
}

fn is_valid_torrent(body: &[u8]) -> bool {
    let mut parser = BencodeParser {
        bytes: body,
        position: 0,
        items: 0,
    };
    parser.parse_torrent().is_ok() && parser.position == body.len()
}

const MAX_BENCODE_DEPTH: usize = 64;
const MAX_BENCODE_ITEMS: usize = 100_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BencodeKind {
    Dictionary,
    Other,
}

struct BencodeParser<'a> {
    bytes: &'a [u8],
    position: usize,
    items: usize,
}

impl BencodeParser<'_> {
    fn parse_torrent(&mut self) -> Result<(), ()> {
        self.expect(b'd')?;
        let mut info_dictionary = false;
        while self.peek() != Some(b'e') {
            let is_info = self.parse_bytes()? == b"info";
            let kind = self.parse_value(1)?;
            if is_info && kind == BencodeKind::Dictionary {
                info_dictionary = true;
            }
        }
        self.expect(b'e')?;
        info_dictionary.then_some(()).ok_or(())
    }

    fn parse_value(&mut self, depth: usize) -> Result<BencodeKind, ()> {
        if depth > MAX_BENCODE_DEPTH || self.items >= MAX_BENCODE_ITEMS {
            return Err(());
        }
        self.items += 1;
        match self.peek() {
            Some(b'i') => {
                self.parse_integer()?;
                Ok(BencodeKind::Other)
            }
            Some(b'l') => {
                self.expect(b'l')?;
                while self.peek() != Some(b'e') {
                    self.parse_value(depth + 1)?;
                }
                self.expect(b'e')?;
                Ok(BencodeKind::Other)
            }
            Some(b'd') => {
                self.expect(b'd')?;
                while self.peek() != Some(b'e') {
                    self.parse_bytes()?;
                    self.parse_value(depth + 1)?;
                }
                self.expect(b'e')?;
                Ok(BencodeKind::Dictionary)
            }
            Some(b'0'..=b'9') => {
                self.parse_bytes()?;
                Ok(BencodeKind::Other)
            }
            _ => Err(()),
        }
    }

    fn parse_integer(&mut self) -> Result<(), ()> {
        self.expect(b'i')?;
        let negative = self.peek() == Some(b'-');
        if negative {
            self.position += 1;
        }
        let start = self.position;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
        let digits = &self.bytes[start..self.position];
        if digits.is_empty()
            || (digits.len() > 1 && digits[0] == b'0')
            || (negative && digits == b"0")
        {
            return Err(());
        }
        digits
            .iter()
            .try_fold(0_u64, |value, byte| {
                value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
            })
            .ok_or(())?;
        self.expect(b'e')
    }

    fn parse_bytes(&mut self) -> Result<&[u8], ()> {
        let length_start = self.position;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
        let digits = &self.bytes[length_start..self.position];
        if digits.is_empty() || (digits.len() > 1 && digits[0] == b'0') {
            return Err(());
        }
        let length = digits
            .iter()
            .try_fold(0_usize, |value, byte| {
                value.checked_mul(10)?.checked_add(usize::from(byte - b'0'))
            })
            .ok_or(())?;
        self.expect(b':')?;
        let end = self.position.checked_add(length).ok_or(())?;
        let value = self.bytes.get(self.position..end).ok_or(())?;
        self.position = end;
        Ok(value)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn expect(&mut self, expected: u8) -> Result<(), ()> {
        if self.peek() != Some(expected) {
            return Err(());
        }
        self.position += 1;
        Ok(())
    }
}

fn parse_source_gdata(
    body: &[u8],
    source: SourceSite,
    requested: &[GalleryIdentity],
) -> Result<Vec<SourceGallery>, SourceError> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|_| SourceError::new(SourceErrorKind::ParseError, "gdata JSON 無法解讀"))?;
    if root.get("error").is_some() {
        return Err(SourceError::new(
            SourceErrorKind::ParseError,
            "gdata 回報錯誤",
        ));
    }
    let values = root
        .get("gmetadata")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            SourceError::new(SourceErrorKind::ParseError, "gdata 缺少 gmetadata array")
        })?;
    let mut galleries = Vec::new();
    for value in values {
        if value.get("error").is_some() {
            continue;
        }
        let Some(gid) = value.get("gid").and_then(Value::as_u64) else {
            continue;
        };
        let Some(token) = string_field(value, "token") else {
            continue;
        };
        if !requested
            .iter()
            .any(|identity| identity.gid == gid && identity.token.eq_ignore_ascii_case(&token))
        {
            continue;
        }
        let Some(title) = string_field(value, "title") else {
            continue;
        };
        let tags = value
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect();
        galleries.push(SourceGallery {
            source,
            gid,
            token,
            title,
            title_jpn: string_field(value, "title_jpn"),
            category: string_field(value, "category").unwrap_or_default(),
            thumb: string_field(value, "thumb"),
            uploader: string_field(value, "uploader"),
            posted: value.get("posted").and_then(value_as_string),
            rating: value
                .get("rating")
                .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok())),
            tags,
            pages: value
                .get("filecount")
                .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
                .and_then(|value| u32::try_from(value).ok()),
        });
    }
    if !values.is_empty() && galleries.is_empty() {
        return Err(SourceError::new(
            SourceErrorKind::ParseError,
            "gdata 沒有可驗證的 requested gallery",
        ));
    }
    Ok(galleries)
}

fn string_field(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn value_as_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn search_next_cursor(body: &str) -> Option<String> {
    search_pager_cursor(body, "nexturl", "next", "")
}

fn search_previous_cursor(body: &str) -> Option<String> {
    search_pager_cursor(body, "prevurl", "prev", "prev:")
}

fn search_pager_cursor(
    body: &str,
    script_variable: &str,
    query_name: &str,
    prefix: &str,
) -> Option<String> {
    let document = Html::parse_document(body);
    let marker = format!("var {script_variable}");
    let scripted = body.split(';').find_map(|statement| {
        let assignment = statement.split_once(&marker)?.1.trim_start();
        Some(javascript_string(assignment.strip_prefix('=')?.trim()))
    });
    if let Some(scripted) = scripted {
        return scripted.and_then(|href| cursor_from_pager_url(&href, query_name, prefix));
    }
    let link_selector =
        Selector::parse(".ptt a[href], .searchnav a[href]").expect("valid pager selector");
    document
        .select(&link_selector)
        .filter_map(|anchor| anchor.value().attr("href"))
        .find_map(|href| cursor_from_pager_url(href, query_name, prefix))
}

fn javascript_string(value: &str) -> Option<String> {
    let quote = value.chars().next()?;
    if !matches!(quote, '\'' | '"') || value.contains('\\') {
        return None;
    }
    value
        .strip_prefix(quote)?
        .strip_suffix(quote)
        .map(str::to_owned)
}

fn cursor_from_pager_url(value: &str, query_name: &str, prefix: &str) -> Option<String> {
    let value = value.replace("&amp;", "&");
    let url = reqwest::Url::parse(EHENTAI_BASE).ok()?.join(&value).ok()?;
    url.query_pairs()
        .find(|(name, _)| name == query_name)
        .map(|(_, value)| value.into_owned())
        .filter(|cursor| {
            (1..=20).contains(&cursor.len()) && cursor.bytes().all(|byte| byte.is_ascii_digit())
        })
        .map(|cursor| format!("{prefix}{cursor}"))
}

pub fn parse_torrent_list(body: &str) -> Result<Vec<GalleryTorrent>, SourceError> {
    let document = Html::parse_document(body);
    let form_selector = Selector::parse("#torrentinfo form").expect("valid form selector");
    let table_selector = Selector::parse("table").expect("valid table selector");
    let row_selector = Selector::parse("tr").expect("valid row selector");
    let link_selector = Selector::parse("a[href]").expect("valid link selector");
    let action_selector = Selector::parse("[onclick]").expect("valid onclick selector");
    let styled_selector = Selector::parse("[style]").expect("valid style selector");
    let mut torrents = Vec::new();
    let mut parser_drift = false;
    for form in document.select(&form_selector) {
        let form_text = node_text(form);
        if form_text.to_ascii_lowercase().contains("expunged") {
            continue;
        }
        let Some(table) = form.select(&table_selector).next() else {
            parser_drift = true;
            continue;
        };
        let all_text = node_text(table);
        let rows = table.select(&row_selector).collect::<Vec<_>>();
        if rows.len() < 3 {
            parser_drift = true;
            continue;
        }
        let posted_at = labeled_value(&all_text, &["Posted:", "Posted"])
            .or_else(|| rows.first().map(|row| node_text(*row)))
            .unwrap_or_default();
        let size = labeled_value(&all_text, &["Size:", "Size"]).unwrap_or_default();
        let seeds = labeled_u32(&all_text, &["Seeds:", "Seeds"]);
        let peers = labeled_u32(&all_text, &["Peers:", "Peers"]);
        let downloads = labeled_u32(
            &all_text,
            &["Downloads:", "Downloads", "Completed:", "Completed"],
        );
        let outdated = all_text.to_ascii_lowercase().contains("outdated")
            || rows.first().is_some_and(|row| {
                row.select(&styled_selector).any(|element| {
                    element
                        .value()
                        .attr("style")
                        .is_some_and(style_declares_red)
                }) || row.value().attr("style").is_some_and(style_declares_red)
            });

        let mut torrent_url = None;
        let mut magnet_url = None;
        let mut name = None;
        for link in table.select(&link_selector) {
            let Some(href) = link.value().attr("href") else {
                continue;
            };
            if href.starts_with("magnet:") {
                magnet_url = Some(href.to_owned());
            } else if is_torrent_link(href) {
                torrent_url = Some(href.to_owned());
                let candidate = node_text(link);
                if !candidate.is_empty() {
                    name = Some(candidate);
                }
            }
        }
        if torrent_url.is_none() {
            torrent_url = table
                .select(&action_selector)
                .filter_map(|element| element.value().attr("onclick"))
                .find_map(url_from_onclick);
        }
        let Some(torrent_url) = torrent_url else {
            parser_drift = true;
            continue;
        };
        if magnet_url.is_none() {
            magnet_url = magnet_from_torrent_url(&torrent_url);
        }
        let name = name
            .or_else(|| rows.get(2).map(|row| node_text(*row)))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "E-Hentai torrent".to_owned());
        torrents.push(GalleryTorrent {
            name,
            posted_at,
            size,
            seeds,
            peers,
            downloads,
            outdated,
            torrent_url,
            magnet_url,
        });
    }
    if parser_drift {
        return Err(SourceError::new(
            SourceErrorKind::ParseError,
            "torrent page 結構已變更，無法安全抽出 torrent URL",
        ));
    }
    torrents.sort_by(|left, right| right.posted_at.cmp(&left.posted_at));
    Ok(torrents)
}

fn style_declares_red(style: &str) -> bool {
    style.split(';').any(|declaration| {
        declaration.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("color") && value.trim().eq_ignore_ascii_case("red")
        })
    })
}

fn node_text(node: ElementRef<'_>) -> String {
    node.text()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn labeled_value(text: &str, labels: &[&str]) -> Option<String> {
    labels.iter().find_map(|label| {
        let start = text.find(label)? + label.len();
        let rest = text[start..].trim_start_matches([' ', ':']);
        let end = [
            " Posted",
            " Size",
            " Seeds",
            " Peers",
            " Downloads",
            " Completed",
        ]
        .iter()
        .filter_map(|next| rest.find(next))
        .min()
        .unwrap_or(rest.len());
        let value = rest[..end].trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn labeled_u32(text: &str, labels: &[&str]) -> u32 {
    labeled_value(text, labels)
        .as_deref()
        .and_then(|value| {
            value
                .split_whitespace()
                .next()?
                .replace(',', "")
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

fn is_torrent_link(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("https://")
        && (lower.contains("ehtracker.org/get")
            || lower.contains("e-hentai.org/torrent")
            || lower.contains("exhentai.org/torrent"))
}

fn url_from_onclick(value: &str) -> Option<String> {
    value
        .split(['\'', '"'])
        .find(|part| is_torrent_link(part.trim()))
        .map(|part| part.trim().to_owned())
}

fn magnet_from_torrent_url(url: &str) -> Option<String> {
    let mut run = String::new();
    for character in url.chars() {
        if character.is_ascii_hexdigit() {
            run.push(character);
            if run.len() == 40 {
                return Some(format!("magnet:?xt=urn:btih:{run}"));
            }
        } else {
            run.clear();
        }
    }
    None
}

fn validate_torrent_url(url: &reqwest::Url) -> Result<(), SourceError> {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return Err(unsafe_torrent_url());
    }
    let allowed = match url.host_str() {
        Some("ehtracker.org") => url.path() == "/get" || url.path().starts_with("/get/"),
        Some("e-hentai.org" | "exhentai.org") => {
            url.path().starts_with("/torrent/")
                || url.path() == "/torrent.php"
                || url.path() == "/gallerytorrents.php"
        }
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(unsafe_torrent_url())
    }
}

fn unsafe_torrent_url() -> SourceError {
    SourceError::new(
        SourceErrorKind::UnsafeTorrentUrl,
        "torrent URL 不在允許的 HTTPS host/path 範圍",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    const SEARCH_HTML: &str = r#"<!doctype html><html><body>
      <a href="/g/123/0123456789/">one</a>
      <a href="/unrelated?next=999">unrelated</a>
      <script>
        var prevurl="https://e-hentai.org/?f_search=x&prev=4127000";
        var nexturl="https://e-hentai.org/?f_search=x&next=4126558";
      </script>
    </body></html>"#;
    const GDATA: &str = r#"{"gmetadata":[{
      "gid":123,"token":"0123456789","title":"Romanized","title_jpn":"日本語",
      "category":"Doujinshi","thumb":"https://ehgt.org/a.jpg","uploader":"alice",
      "posted":"1700000000","rating":"4.75","filecount":"42",
      "tags":["artist:someone","language:japanese"]
    }]}"#;
    const TORRENTS: &str = r#"<!doctype html><html><body><div id="torrentinfo">
      <form><div><table><tr><td><span style="color: red">Posted: 2024-01-01 01:00</span></td><td>Size: 12 MiB</td><td>Seeds: 2 Peers: 3 Downloads: 4</td></tr>
      <tr><td>Uploader: alice</td></tr><tr><td><a href="https://ehtracker.org/get/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/file.torrent">Older</a></td></tr><tr><td>ok</td></tr></table></div></form>
      <form><div><table><tr><td>Posted: 2025-02-02 02:00</td><td>Size: 24 MiB</td><td>Seeds: 9 Peers: 8 Completed: 7</td></tr>
      <tr><td>Uploader: bob</td></tr><tr><td>Newest</td></tr><tr><td><input onclick="document.location='https://exhentai.org/torrent/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.torrent'"></td></tr></table></div></form>
      <form><div><table><tr><td>Expunged</td></tr></table></div></form>
    </div></body></html>"#;

    struct FakeTransport {
        responses: Mutex<VecDeque<Result<TransportResponse, SourceError>>>,
        requested: Mutex<Vec<String>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<TransportResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(Ok).collect()),
                requested: Mutex::new(Vec::new()),
            }
        }
    }

    impl SourceTransport for FakeTransport {
        fn get(
            &self,
            url: &str,
            _: Option<&CookieHeader>,
            _: usize,
        ) -> Result<TransportResponse, SourceError> {
            self.requested
                .lock()
                .expect("requests")
                .push(url.to_owned());
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .expect("fake response")
        }

        fn post_json(
            &self,
            url: &str,
            _: Option<&CookieHeader>,
            _: &str,
            _: usize,
        ) -> Result<TransportResponse, SourceError> {
            self.get(url, None, 0)
        }
    }

    fn response(status: u16, body: impl Into<Vec<u8>>) -> TransportResponse {
        TransportResponse {
            status,
            final_url: EXHENTAI_BASE.to_owned(),
            content_type: Some("text/html".to_owned()),
            location: None,
            body: body.into(),
            exceeded_limit: false,
        }
    }

    fn configured_store() -> CookieStore {
        CookieStore::new(Some(
            CookieHeader::parse("ipb_member_id=1; ipb_pass_hash=secret; igneous=mystery")
                .expect("cookie"),
        ))
    }

    #[test]
    fn session_status_fixtures_cover_access_redirect_status_sad_panda_and_parse() {
        let cases = [
            (
                response(200, "<!doctype html><html>Ex</html>"),
                SessionStatus::Exhentai,
            ),
            (response(302, ""), SessionStatus::InvalidCookie),
            (response(403, "denied"), SessionStatus::InvalidCookie),
            (response(429, "limited"), SessionStatus::RateLimited),
            (response(503, "down"), SessionStatus::ExhentaiUnavailable),
            (
                response(200, "<html><img src='sadpanda.jpg'></html>"),
                SessionStatus::ExhentaiUnavailable,
            ),
            (response(200, "not html"), SessionStatus::ParseError),
        ];
        for (fixture, expected) in cases {
            let source = ReqwestEhentaiSource::with_transport(
                configured_store(),
                FakeTransport::new(vec![fixture]),
            );
            assert_eq!(expected, source.validate_session());
        }
        assert_eq!(
            SessionStatus::NotConfigured,
            ReqwestEhentaiSource::with_transport(
                CookieStore::default(),
                FakeTransport::new(vec![])
            )
            .validate_session()
        );
        let eh_only = CookieStore::new(Some(CookieHeader::parse("nw=1").expect("cookie")));
        assert_eq!(
            SessionStatus::EhentaiOnly,
            ReqwestEhentaiSource::with_transport(
                eh_only,
                FakeTransport::new(vec![response(200, "<!doctype html><html>E-Hentai</html>")])
            )
            .validate_session()
        );
    }

    #[test]
    fn incomplete_cookie_requires_observed_public_access() {
        let incomplete = || CookieStore::new(Some(CookieHeader::parse("nw=1").expect("cookie")));
        let network = FakeTransport {
            responses: Mutex::new(VecDeque::from([Err(SourceError::new(
                SourceErrorKind::NetworkError,
                "offline",
            ))])),
            requested: Mutex::new(Vec::new()),
        };
        assert_eq!(
            SessionStatus::NetworkError,
            ReqwestEhentaiSource::with_transport(incomplete(), network).validate_session()
        );
        assert_eq!(
            SessionStatus::RateLimited,
            ReqwestEhentaiSource::with_transport(
                incomplete(),
                FakeTransport::new(vec![response(429, "limited")])
            )
            .validate_session()
        );
        assert_eq!(
            SessionStatus::ParseError,
            ReqwestEhentaiSource::with_transport(
                incomplete(),
                FakeTransport::new(vec![response(200, "not html")])
            )
            .validate_session()
        );
        assert_eq!(
            SessionStatus::NetworkError,
            ReqwestEhentaiSource::with_transport(
                incomplete(),
                FakeTransport::new(vec![response(503, "down")])
            )
            .validate_session()
        );
        assert_eq!(
            SessionStatus::InvalidCookie,
            ReqwestEhentaiSource::with_transport(
                incomplete(),
                FakeTransport::new(vec![response(403, "forbidden")])
            )
            .validate_session()
        );
    }

    #[test]
    fn session_transport_failure_is_network_error() {
        let transport = FakeTransport {
            responses: Mutex::new(VecDeque::from([Err(SourceError::new(
                SourceErrorKind::NetworkError,
                "offline",
            ))])),
            requested: Mutex::new(Vec::new()),
        };
        let source = ReqwestEhentaiSource::with_transport(configured_store(), transport);
        assert_eq!(SessionStatus::NetworkError, source.validate_session());
    }

    #[test]
    fn search_uses_html_identities_then_independent_gdata_fields() {
        let source = ReqwestEhentaiSource::with_transport(
            CookieStore::default(),
            FakeTransport::new(vec![response(200, SEARCH_HTML), response(200, GDATA)]),
        );
        let result = source
            .search(&GallerySearchQuery {
                query: "different fixture query".to_owned(),
                page: 2,
                cursor: Some("123456".to_owned()),
            })
            .expect("search");
        assert_eq!(SourceSite::Ehentai, result.source);
        assert_eq!(2, result.page);
        assert!(result.has_next);
        assert_eq!(Some("4126558"), result.next_cursor.as_deref());
        assert_eq!(Some("prev:4127000"), result.previous_cursor.as_deref());
        let requested = source.transport.requested.lock().expect("requests");
        assert!(requested[0].contains("next=123456"));
        assert!(!requested[0].contains("page="));
        assert_eq!(1, result.galleries.len());
        let gallery = &result.galleries[0];
        assert_eq!("日本語", gallery.title_jpn.as_deref().unwrap());
        assert_eq!(Some(42), gallery.pages);
        assert_eq!(Some(4.75), gallery.rating);
        assert_eq!("alice", gallery.uploader.as_deref().unwrap());

        drop(requested);
        let previous = ReqwestEhentaiSource::with_transport(
            CookieStore::default(),
            FakeTransport::new(vec![response(200, SEARCH_HTML), response(200, GDATA)]),
        );
        previous
            .search(&GallerySearchQuery {
                query: "fixture".to_owned(),
                page: 1,
                cursor: Some("prev:4127000".to_owned()),
            })
            .expect("previous search");
        assert!(previous.transport.requested.lock().expect("requests")[0].contains("prev=4127000"));
    }

    #[test]
    fn search_cursor_supports_current_script_and_legacy_pager_without_unrelated_links() {
        assert_eq!(Some("4126558".to_owned()), search_next_cursor(SEARCH_HTML));
        assert_eq!(
            Some("123".to_owned()),
            search_next_cursor(
                r#"<a href="?next=999">unrelated</a><table class="ptt"><tr><td><a href="?next=123">Next</a></td></tr></table>"#
            )
        );
        assert_eq!(
            None,
            search_next_cursor(r#"<a href="?next=999">unrelated</a>"#)
        );
        assert_eq!(
            None,
            search_next_cursor(
                r#"<script>var nexturl="";</script><table class="ptt"><tr><td><a href="?next=123">stale</a></td></tr></table>"#
            )
        );
    }

    #[test]
    fn search_rejects_gdata_that_does_not_match_html_identity() {
        let unrelated = GDATA.replace("\"gid\":123", "\"gid\":999");
        let source = ReqwestEhentaiSource::with_transport(
            CookieStore::default(),
            FakeTransport::new(vec![response(200, SEARCH_HTML), response(200, unrelated)]),
        );
        let error = source
            .search(&GallerySearchQuery {
                query: "x".to_owned(),
                page: 2,
                cursor: None,
            })
            .expect_err("unrelated gdata");
        assert_eq!(SourceErrorKind::ParseError, error.kind);
    }

    #[test]
    fn invalid_exhentai_search_falls_back_and_marks_actual_source() {
        let source = ReqwestEhentaiSource::with_transport(
            configured_store(),
            FakeTransport::new(vec![
                response(302, ""),
                response(200, SEARCH_HTML),
                response(200, GDATA),
            ]),
        );
        let result = source
            .search(&GallerySearchQuery {
                query: "x".to_owned(),
                page: 0,
                cursor: None,
            })
            .expect("public fallback");
        assert_eq!(SourceSite::Ehentai, result.source);
        assert!(
            result
                .galleries
                .iter()
                .all(|gallery| gallery.source == SourceSite::Ehentai)
        );
    }

    #[test]
    fn failed_public_fallback_preserves_primary_typed_error() {
        let source = ReqwestEhentaiSource::with_transport(
            configured_store(),
            FakeTransport::new(vec![response(302, ""), response(503, "down")]),
        );
        let error = source
            .search(&GallerySearchQuery {
                query: "x".to_owned(),
                page: 0,
                cursor: None,
            })
            .expect_err("both sources fail");
        assert_eq!(SourceErrorKind::InvalidCookie, error.kind);
    }

    #[test]
    fn detail_and_torrents_fallback_mark_public_source() {
        let sad_panda = response(200, "<!doctype html><html>Sad Panda</html>");
        let gallery_page = response(200, "<!doctype html><html>gallery</html>");
        let detail_source = ReqwestEhentaiSource::with_transport(
            configured_store(),
            FakeTransport::new(vec![sad_panda.clone(), gallery_page, response(200, GDATA)]),
        );
        let detail = detail_source
            .gallery(123, "0123456789")
            .expect("public detail fallback");
        assert_eq!(SourceSite::Ehentai, detail.source);
        assert_eq!(Some(42), detail.pages);
        assert_eq!(Some("https://ehgt.org/a.jpg"), detail.thumb.as_deref());

        let torrent_source = ReqwestEhentaiSource::with_transport(
            configured_store(),
            FakeTransport::new(vec![sad_panda, response(200, TORRENTS)]),
        );
        let torrents = torrent_source
            .torrents(123, "0123456789")
            .expect("public torrent fallback");
        assert_eq!(SourceSite::Ehentai, torrents.source);
        assert_eq!(2, torrents.torrents.len());
    }

    #[test]
    fn torrent_dom_parser_handles_links_onclick_magnet_metrics_and_order() {
        let torrents = parse_torrent_list(TORRENTS).expect("torrent list");
        assert_eq!(2, torrents.len());
        assert_eq!("Newest", torrents[0].name);
        assert_eq!(9, torrents[0].seeds);
        assert_eq!(8, torrents[0].peers);
        assert_eq!(7, torrents[0].downloads);
        assert_eq!("24 MiB", torrents[0].size);
        assert!(!torrents[0].outdated);
        assert_eq!(
            Some("magnet:?xt=urn:btih:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            torrents[0].magnet_url.as_deref()
        );
        assert!(torrents[1].outdated);
    }

    #[test]
    fn torrent_parser_distinguishes_empty_expunged_and_parser_drift() {
        let empty = "<!doctype html><html><div id='torrentinfo'></div></html>";
        assert!(parse_torrent_list(empty).expect("empty list").is_empty());
        let expunged = "<!doctype html><html><div id='torrentinfo'><form><table><tr><td>Expunged</td></tr></table></form></div></html>";
        assert!(
            parse_torrent_list(expunged)
                .expect("expunged list")
                .is_empty()
        );
        let unrelated = "<!doctype html><html><form><table><tr><td>unrelated</td></tr></table></form><div id='torrentinfo'></div></html>";
        assert!(
            parse_torrent_list(unrelated)
                .expect("unrelated form")
                .is_empty()
        );
        let drift = "<!doctype html><html><div id='torrentinfo'><form><table><tr><td>Posted: today</td></tr><tr><td>Uploader</td></tr><tr><td>Name without URL</td></tr></table></form></div></html>";
        assert_eq!(
            SourceErrorKind::ParseError,
            parse_torrent_list(drift).expect_err("parser drift").kind
        );
    }

    #[test]
    fn torrent_download_rejects_ssrf_redirect_html_and_oversize() {
        for unsafe_url in [
            "http://ehtracker.org/get/a",
            "https://evil.example/get/a",
            "https://ehtracker.org.evil.example/get/a",
            "https://ehtracker.org/private/a",
            "https://user:pass@ehtracker.org/get/a",
        ] {
            let source = ReqwestEhentaiSource::with_transport(
                CookieStore::default(),
                FakeTransport::new(vec![]),
            );
            assert_eq!(
                SourceErrorKind::UnsafeTorrentUrl,
                source
                    .download_torrent(unsafe_url)
                    .expect_err("unsafe")
                    .kind
            );
        }

        let mut redirect = response(302, "");
        redirect.location = Some("https://127.0.0.1/private".to_owned());
        let source = ReqwestEhentaiSource::with_transport(
            CookieStore::default(),
            FakeTransport::new(vec![redirect]),
        );
        assert_eq!(
            SourceErrorKind::UnsafeTorrentUrl,
            source
                .download_torrent("https://ehtracker.org/get/a")
                .expect_err("redirect SSRF")
                .kind
        );

        let source = ReqwestEhentaiSource::with_transport(
            CookieStore::default(),
            FakeTransport::new(vec![response(200, "<!doctype html><html>no</html>")]),
        );
        assert_eq!(
            SourceErrorKind::InvalidTorrent,
            source
                .download_torrent("https://ehtracker.org/get/a")
                .expect_err("html")
                .kind
        );

        let mut large = response(200, b"d4:infode".to_vec());
        large.exceeded_limit = true;
        let source = ReqwestEhentaiSource::with_transport(
            CookieStore::default(),
            FakeTransport::new(vec![large]),
        );
        assert_eq!(
            SourceErrorKind::TorrentTooLarge,
            source
                .download_torrent("https://ehtracker.org/get/a")
                .expect_err("large")
                .kind
        );

        for status in [404, 410] {
            let source = ReqwestEhentaiSource::with_transport(
                CookieStore::default(),
                FakeTransport::new(vec![response(status, "missing")]),
            );
            assert_eq!(
                SourceErrorKind::TorrentNotFound,
                source
                    .download_torrent("https://ehtracker.org/get/missing")
                    .expect_err("expired torrent")
                    .kind
            );
        }
    }

    #[test]
    fn valid_torrent_download_accepts_allowed_redirect_and_bencode() {
        let mut redirect = response(302, "");
        redirect.location = Some("/get/final".to_owned());
        let mut torrent = response(200, b"d4:infod4:name4:testee".to_vec());
        torrent.content_type = Some("application/x-bittorrent".to_owned());
        let source = ReqwestEhentaiSource::with_transport(
            CookieStore::default(),
            FakeTransport::new(vec![redirect, torrent]),
        );
        let download = source
            .download_torrent("https://ehtracker.org/get/start")
            .expect("torrent");
        assert!(download.bytes.starts_with(b"d4:info"));
    }

    #[test]
    fn strict_bencode_rejects_truncated_non_dictionary_info_and_trailing_data() {
        assert!(!is_valid_torrent(b"d4:info"));
        assert!(!is_valid_torrent(b"d4:info4:teste"));
        assert!(!is_valid_torrent(b"d4:infodejunk"));
        assert!(is_valid_torrent(b"d4:infod4:name4:testee"));
    }
}
