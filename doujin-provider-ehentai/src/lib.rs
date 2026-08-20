//! E-Hentai/ExHentai gallery metadata provider with exact identity and title lookup.

mod cookie;
mod source;

pub use cookie::{CookieHeader, CookieParseError, CookieStore};
pub use source::{
    EhentaiSource, GalleryDetail, GallerySearchPage, GallerySearchQuery, GalleryTorrent,
    GalleryTorrentList, ReqwestEhentaiSource, ReqwestSourceTransport, SessionStatus, SourceError,
    SourceErrorKind, SourceGallery, SourceSite, SourceTransport, TorrentDownload,
    TransportResponse, parse_torrent_list,
};

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use doujin_app::external_search::{
    ExternalMetadataCandidate, ExternalMetadataProvider, ExternalSearchProviderError,
    ExternalSearchProviderResponse, ExternalSearchRequest, ExternalTagCandidate,
};
use doujin_parser::domain::{Authors, Classification, Parody};
use doujin_storage::jobs::ExternalSearchErrorKind;
use doujin_storage::metadata::{ConfidenceEvidence, MetadataField, MetadataValue};
use reqwest::header::{CONTENT_TYPE, COOKIE};
use scraper::{Html, Selector};
use serde_json::Value;

const PUBLIC_SEARCH_URL: &str = "https://e-hentai.org/";
const EXHENTAI_SEARCH_URL: &str = "https://exhentai.org/";
const API_URL: &str = "https://api.e-hentai.org/api.php";
const USER_AGENT: &str = "doujin-tagger/0.1 (local metadata lookup)";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
const MAX_GALLERY_CANDIDATES: usize = 25;

thread_local! {
    // `ExternalMetadataProvider::search` is synchronous. Remembering the site
    // selected for the last request on that thread keeps the returned source
    // reference consistent if settings are updated concurrently.
    static LAST_METADATA_SITE: Cell<Option<&'static str>> = const { Cell::new(None) };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EhentaiHttpResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EhentaiHttpError {
    pub message: String,
}

pub trait EhentaiHttpClient: Send + Sync {
    fn search_title(&self, title: &str) -> Result<EhentaiHttpResponse, EhentaiHttpError>;
    fn fetch_metadata(
        &self,
        galleries: &[GalleryIdentity],
    ) -> Result<EhentaiHttpResponse, EhentaiHttpError>;
    fn gallery_base_url(&self) -> &'static str;
}

#[derive(Debug)]
pub struct EhentaiClientBuildError(String);

impl fmt::Display for EhentaiClientBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "無法建立 E-Hentai HTTP client：{}", self.0)
    }
}

impl Error for EhentaiClientBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

pub struct ReqwestEhentaiClient {
    client: reqwest::blocking::Client,
    cookies: CookieStore,
    request_gate: Mutex<RequestGate>,
}

#[derive(Debug)]
struct RequestGate {
    last_started: Option<Instant>,
    min_interval: Duration,
}

impl ReqwestEhentaiClient {
    pub fn new() -> Result<Self, EhentaiClientBuildError> {
        let configured_cookie = std::env::var("DOUJIN_EXHENTAI_COOKIE")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let cookie = configured_cookie
            .as_deref()
            .map(CookieHeader::parse)
            .transpose()
            .map_err(|error| EhentaiClientBuildError(error.to_string()))?;
        Self::with_cookie_store(CookieStore::new(cookie))
    }

    pub fn with_cookie_store(cookies: CookieStore) -> Result<Self, EhentaiClientBuildError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| EhentaiClientBuildError(error.to_string()))?;
        Ok(Self {
            client,
            cookies,
            request_gate: Mutex::new(RequestGate {
                last_started: None,
                min_interval: DEFAULT_MIN_REQUEST_INTERVAL,
            }),
        })
    }

    fn fetch(
        &self,
        mut request: reqwest::blocking::RequestBuilder,
        cookie: Option<&CookieHeader>,
    ) -> Result<EhentaiHttpResponse, EhentaiHttpError> {
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie.request_header_value());
        } else {
            request = request.header(COOKIE, "nw=1");
        }
        let mut gate = self.request_gate.lock().map_err(|_| EhentaiHttpError {
            message: "E-Hentai request gate 已失效".to_owned(),
        })?;
        if let Some(last_started) = gate.last_started {
            let elapsed = last_started.elapsed();
            if elapsed < gate.min_interval {
                thread::sleep(gate.min_interval - elapsed);
            }
        }
        gate.last_started = Some(Instant::now());
        let response = request.send().map_err(|error| EhentaiHttpError {
            message: error.to_string(),
        })?;
        let status = response.status().as_u16();
        let body = response.text().map_err(|error| EhentaiHttpError {
            message: error.to_string(),
        })?;
        Ok(EhentaiHttpResponse { status, body })
    }
}

impl EhentaiHttpClient for ReqwestEhentaiClient {
    fn search_title(&self, title: &str) -> Result<EhentaiHttpResponse, EhentaiHttpError> {
        let cookie = self.cookies.snapshot();
        let (search_url, gallery_base_url) = if cookie.is_some() {
            (EXHENTAI_SEARCH_URL, "https://exhentai.org")
        } else {
            (PUBLIC_SEARCH_URL, "https://e-hentai.org")
        };
        LAST_METADATA_SITE.set(Some(gallery_base_url));
        self.fetch(
            self.client.get(search_url).query(&[("f_search", title)]),
            cookie.as_ref(),
        )
    }

    fn fetch_metadata(
        &self,
        galleries: &[GalleryIdentity],
    ) -> Result<EhentaiHttpResponse, EhentaiHttpError> {
        let cookie = self.cookies.snapshot();
        LAST_METADATA_SITE.set(Some(if cookie.is_some() {
            "https://exhentai.org"
        } else {
            "https://e-hentai.org"
        }));
        let body = serde_json::json!({
            "method": "gdata",
            "gidlist": galleries
                .iter()
                .map(|gallery| serde_json::json!([gallery.gid, gallery.token]))
                .collect::<Vec<_>>(),
            "namespace": 1,
        })
        .to_string();
        self.fetch(
            self.client
                .post(API_URL)
                .header(CONTENT_TYPE, "application/json")
                .body(body),
            cookie.as_ref(),
        )
    }

    fn gallery_base_url(&self) -> &'static str {
        LAST_METADATA_SITE.get().unwrap_or_else(|| {
            if self.cookies.is_configured() {
                "https://exhentai.org"
            } else {
                "https://e-hentai.org"
            }
        })
    }
}

pub struct EhentaiProvider<C> {
    client: C,
}

impl EhentaiProvider<ReqwestEhentaiClient> {
    pub fn production() -> Result<Self, EhentaiClientBuildError> {
        Ok(Self::with_client(ReqwestEhentaiClient::new()?))
    }

    pub fn production_with_cookie_store(
        cookies: CookieStore,
    ) -> Result<Self, EhentaiClientBuildError> {
        Ok(Self::with_client(ReqwestEhentaiClient::with_cookie_store(
            cookies,
        )?))
    }
}

impl<C> EhentaiProvider<C> {
    pub fn with_client(client: C) -> Self {
        Self { client }
    }
}

impl<C: EhentaiHttpClient> ExternalMetadataProvider for EhentaiProvider<C> {
    fn search(
        &self,
        request: &ExternalSearchRequest,
    ) -> Result<ExternalSearchProviderResponse, ExternalSearchProviderError> {
        let (gallery, lookup) = match unique_gallery_identity(request)? {
            Some(identity) => {
                let response = self.fetch_galleries(std::slice::from_ref(&identity))?;
                let gallery = response
                    .into_iter()
                    .find(|gallery| gallery.identity == identity)
                    .ok_or_else(|| {
                        provider_error(
                            ExternalSearchErrorKind::NoMatch,
                            format!("E-Hentai gdata 沒有回傳 gallery {}", identity.gid),
                        )
                    })?;
                (gallery, LookupMatch::ExactGallery)
            }
            None => {
                let title = recognized_title(request)?;
                let search = self.client.search_title(title).map_err(|error| {
                    provider_error(ExternalSearchErrorKind::Network, error.message)
                })?;
                classify_status(search.status, "E-Hentai 書名搜尋")?;
                let identities = gallery_identities_from_search(&search.body)?;
                if identities.is_empty() {
                    return Err(provider_error(
                        ExternalSearchErrorKind::NoMatch,
                        format!("E-Hentai 找不到與書名「{title}」相符的 gallery"),
                    ));
                }
                let galleries = self.fetch_galleries(&identities)?;
                let gallery = exact_title_gallery(galleries, title)?;
                (gallery, LookupMatch::ExactTitle)
            }
        };
        Ok(map_gallery(
            request,
            gallery,
            lookup,
            self.client.gallery_base_url(),
        ))
    }
}

impl<C: EhentaiHttpClient> EhentaiProvider<C> {
    fn fetch_galleries(
        &self,
        identities: &[GalleryIdentity],
    ) -> Result<Vec<GalleryMetadata>, ExternalSearchProviderError> {
        let response = self
            .client
            .fetch_metadata(identities)
            .map_err(|error| provider_error(ExternalSearchErrorKind::Network, error.message))?;
        classify_status(response.status, "E-Hentai gdata")?;
        parse_gdata(&response.body, identities)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GalleryIdentity {
    gid: u64,
    token: String,
}

impl GalleryIdentity {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim().trim_matches('/');
        let mut parts = value.split('/');
        let gid = parts.next()?.parse().ok()?;
        let token = parts.next()?.to_ascii_lowercase();
        if parts.next().is_some()
            || token.len() != 10
            || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        Some(Self { gid, token })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LookupMatch {
    ExactGallery,
    ExactTitle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GalleryMetadata {
    identity: GalleryIdentity,
    title: String,
    title_jpn: Option<String>,
    category: String,
    tags: Vec<String>,
}

fn unique_gallery_identity(
    request: &ExternalSearchRequest,
) -> Result<Option<GalleryIdentity>, ExternalSearchProviderError> {
    let galleries = request
        .identifiers
        .iter()
        .filter(|identifier| {
            matches!(
                identifier.scheme.to_ascii_uppercase().as_str(),
                "EH" | "E-HENTAI" | "EXHENTAI"
            )
        })
        .filter_map(|identifier| GalleryIdentity::parse(&identifier.value))
        .collect::<HashSet<_>>();
    match galleries.len() {
        0 => Ok(None),
        1 => Ok(galleries.into_iter().next()),
        count => Err(provider_error(
            ExternalSearchErrorKind::Unsupported,
            format!("收藏具有 {count} 個不同 E-Hentai gallery 識別碼，拒絕自動選擇"),
        )),
    }
}

fn recognized_title(request: &ExternalSearchRequest) -> Result<&str, ExternalSearchProviderError> {
    request
        .collection
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or_else(|| {
            provider_error(
                ExternalSearchErrorKind::Unsupported,
                "收藏沒有可用的 E-Hentai gallery 識別碼或辨識書名".to_owned(),
            )
        })
}

fn gallery_identities_from_search(
    body: &str,
) -> Result<Vec<GalleryIdentity>, ExternalSearchProviderError> {
    let document = Html::parse_document(body);
    let selector = Selector::parse("a[href]").expect("valid gallery link selector");
    let mut identities = Vec::new();
    let mut seen = HashSet::new();
    for link in document.select(&selector) {
        let Some(href) = link.value().attr("href") else {
            continue;
        };
        let Some(identity) = gallery_identity_from_url(href) else {
            continue;
        };
        if seen.insert(identity.clone()) {
            identities.push(identity);
            if identities.len() == MAX_GALLERY_CANDIDATES {
                break;
            }
        }
    }
    Ok(identities)
}

fn gallery_identity_from_url(value: &str) -> Option<GalleryIdentity> {
    let marker = "/g/";
    let start = value.find(marker)? + marker.len();
    GalleryIdentity::parse(&value[start..])
}

fn parse_gdata(
    body: &str,
    requested: &[GalleryIdentity],
) -> Result<Vec<GalleryMetadata>, ExternalSearchProviderError> {
    let root: Value = serde_json::from_str(body).map_err(|error| {
        provider_error(
            ExternalSearchErrorKind::InvalidResponse,
            format!("E-Hentai gdata JSON 無法解讀：{error}"),
        )
    })?;
    if let Some(error) = root.get("error").and_then(Value::as_str) {
        return Err(provider_error(
            ExternalSearchErrorKind::InvalidResponse,
            format!("E-Hentai gdata 回報錯誤：{error}"),
        ));
    }
    let values = root
        .get("gmetadata")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            provider_error(
                ExternalSearchErrorKind::InvalidResponse,
                "E-Hentai gdata response 缺少 gmetadata array".to_owned(),
            )
        })?;
    let tokens = requested
        .iter()
        .map(|identity| (identity.gid, identity.token.clone()))
        .collect::<HashMap<_, _>>();
    let mut galleries = Vec::new();
    for value in values {
        let Some(object) = value.as_object() else {
            continue;
        };
        if object.get("error").is_some() {
            continue;
        }
        let Some(gid) = object.get("gid").and_then(Value::as_u64) else {
            continue;
        };
        let token = object
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| tokens.get(&gid).cloned());
        let Some(token) = token else {
            continue;
        };
        let Some(title) = object
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
        else {
            continue;
        };
        let title_jpn = object
            .get("title_jpn")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned);
        let category = object
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let tags = object
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect();
        galleries.push(GalleryMetadata {
            identity: GalleryIdentity { gid, token },
            title: title.to_owned(),
            title_jpn,
            category,
            tags,
        });
    }
    Ok(galleries)
}

fn exact_title_gallery(
    galleries: Vec<GalleryMetadata>,
    query: &str,
) -> Result<GalleryMetadata, ExternalSearchProviderError> {
    let normalized_query = normalize_title(query);
    let mut matching = galleries
        .into_iter()
        .filter(|gallery| {
            gallery_titles(gallery)
                .into_iter()
                .any(|title| normalize_title(title) == normalized_query)
        })
        .collect::<Vec<_>>();
    match matching.len() {
        1 => Ok(matching.remove(0)),
        0 => Err(provider_error(
            ExternalSearchErrorKind::NoMatch,
            format!("E-Hentai 搜尋結果沒有與書名「{query}」完全相符的 gallery"),
        )),
        count => Err(provider_error(
            ExternalSearchErrorKind::NoMatch,
            format!("E-Hentai 搜尋結果有 {count} 筆與書名「{query}」完全相符，拒絕依排名選擇"),
        )),
    }
}

fn gallery_titles(gallery: &GalleryMetadata) -> Vec<&str> {
    let mut titles = Vec::with_capacity(4);
    for title in [Some(gallery.title.as_str()), gallery.title_jpn.as_deref()]
        .into_iter()
        .flatten()
    {
        titles.push(title);
        if let Some(stripped) = strip_leading_bracket(title) {
            titles.push(stripped);
        }
    }
    titles
}

fn strip_leading_bracket(value: &str) -> Option<&str> {
    let value = value.trim();
    let closing = value.strip_prefix('[')?.find(']')?;
    let remaining = value[closing + 2..].trim();
    (!remaining.is_empty()).then_some(remaining)
}

fn normalize_title(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn map_gallery(
    request: &ExternalSearchRequest,
    gallery: GalleryMetadata,
    lookup: LookupMatch,
    gallery_base_url: &str,
) -> ExternalSearchProviderResponse {
    let source_reference = format!(
        "{gallery_base_url}/g/{}/{}/",
        gallery.identity.gid, gallery.identity.token
    );
    let confidence = confidence(lookup, gallery.identity.gid);
    let mut candidates = Vec::new();
    let mut mapped_namespaces = HashSet::new();

    if request.fields.contains(&MetadataField::Title) {
        let title = gallery
            .title_jpn
            .as_deref()
            .or(Some(gallery.title.as_str()))
            .and_then(|title| strip_leading_bracket(title).or(Some(title)))
            .unwrap_or_default()
            .trim();
        if !title.is_empty() {
            candidates.push(metadata_candidate(
                MetadataField::Title,
                MetadataValue::Text(title.to_owned()),
                &source_reference,
                &confidence,
            ));
        }
    }

    let namespaced = gallery
        .tags
        .iter()
        .filter_map(|tag| tag.split_once(':'))
        .map(|(namespace, value)| (namespace.trim(), value.trim()))
        .filter(|(_, value)| !value.is_empty())
        .collect::<Vec<_>>();

    if request.fields.contains(&MetadataField::Circle) {
        let groups = namespace_values(&namespaced, "group");
        if groups.len() == 1 {
            mapped_namespaces.insert("group");
            candidates.push(metadata_candidate(
                MetadataField::Circle,
                MetadataValue::Text(groups[0].to_owned()),
                &source_reference,
                &confidence,
            ));
        }
    }

    if request.fields.contains(&MetadataField::Authors) {
        let artists = namespace_values(&namespaced, "artist");
        if !artists.is_empty() {
            mapped_namespaces.insert("artist");
            candidates.push(metadata_candidate(
                MetadataField::Authors,
                MetadataValue::Authors(Authors {
                    raw: Some(artists.join(", ")),
                    values: artists.into_iter().map(str::to_owned).collect(),
                }),
                &source_reference,
                &confidence,
            ));
        }
    }

    if request.fields.contains(&MetadataField::Parody) {
        let parodies = namespace_values(&namespaced, "parody");
        if parodies.len() == 1 {
            mapped_namespaces.insert("parody");
            let raw = parodies[0];
            candidates.push(metadata_candidate(
                MetadataField::Parody,
                MetadataValue::Parody(Parody {
                    raw: raw.to_owned(),
                    canonical: if raw.eq_ignore_ascii_case("original") {
                        "オリジナル".to_owned()
                    } else {
                        raw.to_owned()
                    },
                    evidence: "E-Hentai parody namespace".to_owned(),
                }),
                &source_reference,
                &confidence,
            ));
        }
    }

    if request.fields.contains(&MetadataField::Classification) {
        let top_level = match gallery.category.as_str() {
            "Doujinshi" => Some("同人誌"),
            "Manga" => Some("商業誌"),
            "Artist CG" | "Game CG" | "Image Set" => Some("CG"),
            _ => None,
        };
        if let Some(top_level) = top_level {
            candidates.push(metadata_candidate(
                MetadataField::Classification,
                MetadataValue::Classification(Classification {
                    top_level: top_level.to_owned(),
                    subcategory: None,
                    raw_marker: Some(gallery.category.clone()),
                }),
                &source_reference,
                &confidence,
            ));
        }
    }

    let allowed_tag_namespaces = ["character", "female", "male", "mixed", "other", "language"];
    let tags = namespaced
        .into_iter()
        .filter(|(namespace, _)| allowed_tag_namespaces.contains(namespace))
        .map(|(namespace, value)| format!("{namespace}:{value}"))
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|name| ExternalTagCandidate {
            name,
            source_reference: source_reference.clone(),
            confidence: confidence.clone(),
        })
        .collect();

    ExternalSearchProviderResponse {
        candidates,
        tags,
        issues: Vec::new(),
    }
}

fn namespace_values<'a>(values: &[(&'a str, &'a str)], namespace: &str) -> Vec<&'a str> {
    let mut result = values
        .iter()
        .filter(|(candidate, _)| *candidate == namespace)
        .map(|(_, value)| *value)
        .collect::<Vec<_>>();
    result.sort_unstable();
    result.dedup();
    result
}

fn metadata_candidate(
    field: MetadataField,
    value: MetadataValue,
    source_reference: &str,
    confidence: &ConfidenceEvidence,
) -> ExternalMetadataCandidate {
    ExternalMetadataCandidate {
        field,
        value,
        source_reference: source_reference.to_owned(),
        confidence: confidence.clone(),
    }
}

fn confidence(lookup: LookupMatch, gid: u64) -> ConfidenceEvidence {
    match lookup {
        LookupMatch::ExactGallery => ConfidenceEvidence {
            total: 0.97,
            source_reliability: 0.9,
            identifier_match: 1.0,
            string_similarity: 0.95,
            rule_certainty: 0.97,
            reliable_identifier_exact_match: true,
            reason: format!("E-Hentai gallery {gid} 的 gid/token 完全匹配"),
        },
        LookupMatch::ExactTitle => ConfidenceEvidence {
            total: 0.88,
            source_reliability: 0.85,
            identifier_match: 0.5,
            string_similarity: 1.0,
            rule_certainty: 0.9,
            reliable_identifier_exact_match: false,
            reason: format!("E-Hentai gallery {gid} 的日文或羅馬字書名唯一完全匹配"),
        },
    }
}

fn classify_status(status: u16, operation: &str) -> Result<(), ExternalSearchProviderError> {
    match status {
        200 => Ok(()),
        404 => Err(provider_error(
            ExternalSearchErrorKind::NoMatch,
            format!("{operation} 回傳 HTTP 404"),
        )),
        429 => Err(provider_error(
            ExternalSearchErrorKind::RateLimited,
            format!("{operation} 回傳 HTTP 429"),
        )),
        500..=599 => Err(provider_error(
            ExternalSearchErrorKind::ProviderUnavailable,
            format!("{operation} 回傳 HTTP {status}"),
        )),
        400 | 401 | 403 => Err(provider_error(
            ExternalSearchErrorKind::Unsupported,
            format!("{operation} 被拒絕：HTTP {status}"),
        )),
        _ => Err(provider_error(
            ExternalSearchErrorKind::InvalidResponse,
            format!("{operation} 回傳未支援的 HTTP status：{status}"),
        )),
    }
}

fn provider_error(kind: ExternalSearchErrorKind, message: String) -> ExternalSearchProviderError {
    ExternalSearchProviderError { kind, message }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use doujin_parser::domain::Identifier;
    use doujin_storage::collections::CollectionSnapshot;

    use super::*;

    const SEARCH: &str = r#"
      <table><tr><td><a href="https://e-hentai.org/g/3803778/1d181d61b9/"><div class="glink">translated</div></a></td></tr>
      <tr><td><a href="https://e-hentai.org/g/3733723/051033d387/"><div class="glink">original</div></a></td></tr></table>
    "#;
    const GDATA: &str = r#"{
      "gmetadata": [
        {
          "gid": 3803778, "token": "1d181d61b9",
          "title": "Kyonyuu Mama (Spanish) MTL",
          "title_jpn": "[殿様ペンギン] 巨乳ママとメンヘラ彼女を親子丼にして孕ませる話・終 [スペイン翻訳]",
          "category": "Doujinshi",
          "tags": ["language:spanish", "language:translated", "parody:original", "group:tonosama penguin"]
        },
        {
          "gid": 3733723, "token": "051033d387",
          "title": "[Tonosama Penguin] Kyonyuu Mama to Menhera Kanojo",
          "title_jpn": "[殿様ペンギン] 巨乳ママとメンヘラ彼女を親子丼にして孕ませる話・終",
          "category": "Doujinshi",
          "tags": ["parody:original", "group:tonosama penguin", "artist:penguin", "female:big breasts", "female:milf", "mixed:ffm threesome", "other:mosaic censorship"]
        }
      ]
    }"#;

    struct FakeClient {
        search: EhentaiHttpResponse,
        metadata: EhentaiHttpResponse,
    }

    impl EhentaiHttpClient for FakeClient {
        fn search_title(&self, _: &str) -> Result<EhentaiHttpResponse, EhentaiHttpError> {
            Ok(self.search.clone())
        }

        fn fetch_metadata(
            &self,
            _: &[GalleryIdentity],
        ) -> Result<EhentaiHttpResponse, EhentaiHttpError> {
            Ok(self.metadata.clone())
        }

        fn gallery_base_url(&self) -> &'static str {
            "https://e-hentai.org"
        }
    }

    fn request(identifiers: Vec<Identifier>) -> ExternalSearchRequest {
        ExternalSearchRequest {
            job_id: 1,
            collection: CollectionSnapshot {
                id: 9,
                path: PathBuf::from("H:/book.zip"),
                filename: "book.zip".to_owned(),
                root: None,
                title: Some("巨乳ママとメンヘラ彼女を親子丼にして孕ませる話・終".to_owned()),
                event: None,
                circle: None,
                authors: Vec::new(),
                parody: None,
                parody_raw: None,
                classification_top: Some("同人誌".to_owned()),
                classification_subcategory: None,
                is_dl: None,
                tags: Vec::new(),
                created_at: String::new(),
                updated_at: String::new(),
            },
            identifiers,
            fields: vec![
                MetadataField::Title,
                MetadataField::Circle,
                MetadataField::Authors,
                MetadataField::Parody,
                MetadataField::Classification,
            ],
        }
    }

    #[test]
    fn title_lookup_selects_original_gallery_and_maps_namespaces() {
        let provider = EhentaiProvider::with_client(FakeClient {
            search: EhentaiHttpResponse {
                status: 200,
                body: SEARCH.to_owned(),
            },
            metadata: EhentaiHttpResponse {
                status: 200,
                body: GDATA.to_owned(),
            },
        });
        let response = provider.search(&request(Vec::new())).expect("title lookup");

        assert_eq!(5, response.candidates.len());
        assert!(response.candidates.iter().all(|candidate| {
            candidate.source_reference == "https://e-hentai.org/g/3733723/051033d387/"
                && !candidate.confidence.reliable_identifier_exact_match
        }));
        assert!(response.candidates.iter().any(|candidate| {
            candidate.field == MetadataField::Parody
                && matches!(&candidate.value, MetadataValue::Parody(value) if value.canonical == "オリジナル")
        }));
        let tags = response
            .tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(4, tags.len());
        assert!(tags.contains("female:big breasts"));
        assert!(tags.contains("mixed:ffm threesome"));
        assert!(!tags.contains("group:tonosama penguin"));
    }

    #[test]
    fn typed_gallery_identity_is_reliable_and_skips_title_search_semantics() {
        let provider = EhentaiProvider::with_client(FakeClient {
            search: EhentaiHttpResponse {
                status: 500,
                body: String::new(),
            },
            metadata: EhentaiHttpResponse {
                status: 200,
                body: GDATA.to_owned(),
            },
        });
        let response = provider
            .search(&request(vec![Identifier {
                scheme: "EXHENTAI".to_owned(),
                value: "3733723/051033d387".to_owned(),
                raw: "https://exhentai.org/g/3733723/051033d387/".to_owned(),
            }]))
            .expect("identity lookup");
        assert!(response.candidates.iter().all(|candidate| {
            candidate.confidence.total >= 0.95
                && candidate.confidence.reliable_identifier_exact_match
        }));
    }

    #[test]
    fn multiple_exact_title_galleries_remain_ambiguous() {
        let galleries = vec![
            GalleryMetadata {
                identity: GalleryIdentity::parse("1/0123456789").expect("identity"),
                title: "[Circle] Same".to_owned(),
                title_jpn: None,
                category: "Doujinshi".to_owned(),
                tags: Vec::new(),
            },
            GalleryMetadata {
                identity: GalleryIdentity::parse("2/abcdef1234").expect("identity"),
                title: "[Circle] Same".to_owned(),
                title_jpn: None,
                category: "Doujinshi".to_owned(),
                tags: Vec::new(),
            },
        ];
        let error = exact_title_gallery(galleries, "Same").expect_err("ambiguous title");
        assert_eq!(ExternalSearchErrorKind::NoMatch, error.kind);
        assert!(error.message.contains("2 筆"));
    }

    #[test]
    fn production_metadata_client_observes_shared_cookie_updates() {
        let store = CookieStore::default();
        let client = ReqwestEhentaiClient::with_cookie_store(store.clone()).expect("client");
        assert_eq!("https://e-hentai.org", client.gallery_base_url());

        store.set(Some(
            CookieHeader::parse("ipb_member_id=1; ipb_pass_hash=secret").expect("cookie"),
        ));
        assert_eq!("https://exhentai.org", client.gallery_base_url());

        store.set(None);
        assert_eq!("https://e-hentai.org", client.gallery_base_url());
    }
}
