//! RJ-first DLsite metadata provider with a conservative title fallback.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use doujin_app::external_search::{
    ExternalMetadataCandidate, ExternalMetadataProvider, ExternalSearchProviderError,
    ExternalSearchProviderIssue, ExternalSearchProviderResponse, ExternalSearchRequest,
};
use doujin_parser::domain::{Authors, Parody};
use doujin_storage::jobs::ExternalSearchErrorKind;
use doujin_storage::metadata::{ConfidenceEvidence, MetadataField, MetadataValue};
use reqwest::header::{COOKIE, HeaderMap, HeaderValue};
use scraper::{Html, Selector};
use serde_json::{Map, Value};

const PRODUCT_API_URL: &str = "https://www.dlsite.com/maniax/api/=/product.json";
const PRODUCT_PAGE_BASE: &str = "https://www.dlsite.com";
const TITLE_SEARCH_URL: &str = "https://www.dlsite.com/maniax/fsr/=/keyword/";
const USER_AGENT: &str = "doujin-tagger/0.1 (local metadata lookup)";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlsiteHttpResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlsiteHttpError {
    pub message: String,
}

pub trait DlsiteHttpClient: Send + Sync {
    fn fetch_product(&self, rj: &str) -> Result<DlsiteHttpResponse, DlsiteHttpError>;
    fn search_title(&self, title: &str) -> Result<DlsiteHttpResponse, DlsiteHttpError>;
}

#[derive(Debug)]
pub struct DlsiteClientBuildError(reqwest::Error);

impl fmt::Display for DlsiteClientBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "無法建立 DLsite HTTP client：{}", self.0)
    }
}

impl Error for DlsiteClientBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.0)
    }
}

pub struct ReqwestDlsiteClient {
    client: reqwest::blocking::Client,
    request_gate: Mutex<RequestGate>,
}

#[derive(Debug)]
struct RequestGate {
    last_started: Option<Instant>,
    min_interval: Duration,
}

impl ReqwestDlsiteClient {
    pub fn new() -> Result<Self, DlsiteClientBuildError> {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static("adultchecked=1"));
        let client = reqwest::blocking::Client::builder()
            .default_headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(DlsiteClientBuildError)?;
        Ok(Self {
            client,
            request_gate: Mutex::new(RequestGate {
                last_started: None,
                min_interval: DEFAULT_MIN_REQUEST_INTERVAL,
            }),
        })
    }

    fn fetch(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<DlsiteHttpResponse, DlsiteHttpError> {
        let mut gate = self.request_gate.lock().map_err(|_| DlsiteHttpError {
            message: "DLsite request gate 已失效".to_owned(),
        })?;
        if let Some(last_started) = gate.last_started {
            let elapsed = last_started.elapsed();
            if elapsed < gate.min_interval {
                thread::sleep(gate.min_interval - elapsed);
            }
        }
        gate.last_started = Some(Instant::now());
        let response = request.send().map_err(|error| DlsiteHttpError {
            message: error.to_string(),
        })?;
        let status = response.status().as_u16();
        let body = response.text().map_err(|error| DlsiteHttpError {
            message: error.to_string(),
        })?;
        Ok(DlsiteHttpResponse { status, body })
    }
}

impl DlsiteHttpClient for ReqwestDlsiteClient {
    fn fetch_product(&self, rj: &str) -> Result<DlsiteHttpResponse, DlsiteHttpError> {
        self.fetch(self.client.get(PRODUCT_API_URL).query(&[("workno", rj)]))
    }

    fn search_title(&self, title: &str) -> Result<DlsiteHttpResponse, DlsiteHttpError> {
        self.fetch(self.client.get(title_search_url(title)?))
    }
}

fn title_search_url(title: &str) -> Result<reqwest::Url, DlsiteHttpError> {
    let mut url = reqwest::Url::parse(TITLE_SEARCH_URL).map_err(|error| DlsiteHttpError {
        message: format!("DLsite 書名搜尋 URL 無效：{error}"),
    })?;
    url.path_segments_mut()
        .map_err(|_| DlsiteHttpError {
            message: "DLsite 書名搜尋 URL 無法加入路徑".to_owned(),
        })?
        .pop_if_empty()
        .push(title)
        .push("");
    Ok(url)
}

pub struct DlsiteExactProvider<C> {
    client: C,
}

impl DlsiteExactProvider<ReqwestDlsiteClient> {
    pub fn production() -> Result<Self, DlsiteClientBuildError> {
        Ok(Self::with_client(ReqwestDlsiteClient::new()?))
    }
}

impl<C> DlsiteExactProvider<C> {
    pub fn with_client(client: C) -> Self {
        Self { client }
    }
}

impl<C: DlsiteHttpClient> ExternalMetadataProvider for DlsiteExactProvider<C> {
    fn search(
        &self,
        request: &ExternalSearchRequest,
    ) -> Result<ExternalSearchProviderResponse, ExternalSearchProviderError> {
        let lookup = match unique_rj(request)? {
            Some(rj) => LookupMatch::ExactRj { rj },
            None => LookupMatch::ExactTitle {
                rj: search_rj_by_title(&self.client, request)?,
                query: recognized_title(request)?.to_owned(),
            },
        };
        let rj = lookup.rj();
        let response = self
            .client
            .fetch_product(rj)
            .map_err(|error| provider_error(ExternalSearchErrorKind::Network, error.message))?;
        match response.status {
            200 => {}
            404 => {
                return Err(provider_error(
                    ExternalSearchErrorKind::NoMatch,
                    format!("DLsite 找不到產品 {rj}"),
                ));
            }
            429 => {
                return Err(provider_error(
                    ExternalSearchErrorKind::RateLimited,
                    "DLsite 回傳 HTTP 429".to_owned(),
                ));
            }
            500..=599 => {
                return Err(provider_error(
                    ExternalSearchErrorKind::ProviderUnavailable,
                    format!("DLsite 回傳 HTTP {}", response.status),
                ));
            }
            400 | 401 | 403 => {
                return Err(provider_error(
                    ExternalSearchErrorKind::Unsupported,
                    format!("DLsite 拒絕 request：HTTP {}", response.status),
                ));
            }
            status => {
                return Err(provider_error(
                    ExternalSearchErrorKind::InvalidResponse,
                    format!("DLsite 回傳未支援的 HTTP status：{status}"),
                ));
            }
        }
        let product = exact_product(&response.body, rj)?;
        Ok(map_product(request, rj, &product, &lookup))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LookupMatch {
    ExactRj { rj: String },
    ExactTitle { rj: String, query: String },
}

impl LookupMatch {
    fn rj(&self) -> &str {
        match self {
            Self::ExactRj { rj } | Self::ExactTitle { rj, .. } => rj,
        }
    }
}

fn unique_rj(
    request: &ExternalSearchRequest,
) -> Result<Option<String>, ExternalSearchProviderError> {
    let identifiers = request
        .identifiers
        .iter()
        .filter(|identifier| identifier.scheme == "RJ" && valid_rj(&identifier.value))
        .map(|identifier| identifier.value.clone())
        .collect::<HashSet<_>>();
    match identifiers.len() {
        1 => Ok(Some(
            identifiers.into_iter().next().expect("one RJ identifier"),
        )),
        0 => Ok(None),
        count => Err(provider_error(
            ExternalSearchErrorKind::Unsupported,
            format!("收藏具有 {count} 個不同 RJ 識別碼，拒絕自動選擇"),
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
                "收藏沒有可用的 typed RJ 識別碼或辨識書名".to_owned(),
            )
        })
}

fn search_rj_by_title<C: DlsiteHttpClient>(
    client: &C,
    request: &ExternalSearchRequest,
) -> Result<String, ExternalSearchProviderError> {
    let title = recognized_title(request)?;
    let search_query = title_search_query(title);
    let response = client
        .search_title(search_query)
        .map_err(|error| provider_error(ExternalSearchErrorKind::Network, error.message))?;
    match response.status {
        200 => exact_title_result(&response.body, title),
        404 => Err(provider_error(
            ExternalSearchErrorKind::NoMatch,
            format!("DLsite 找不到與書名「{title}」相符的作品"),
        )),
        429 => Err(provider_error(
            ExternalSearchErrorKind::RateLimited,
            "DLsite 書名搜尋回傳 HTTP 429".to_owned(),
        )),
        500..=599 => Err(provider_error(
            ExternalSearchErrorKind::ProviderUnavailable,
            format!("DLsite 書名搜尋回傳 HTTP {}", response.status),
        )),
        400 | 401 | 403 => Err(provider_error(
            ExternalSearchErrorKind::Unsupported,
            format!("DLsite 拒絕書名搜尋 request：HTTP {}", response.status),
        )),
        status => Err(provider_error(
            ExternalSearchErrorKind::InvalidResponse,
            format!("DLsite 書名搜尋回傳未支援的 HTTP status：{status}"),
        )),
    }
}

fn title_search_query(title: &str) -> &str {
    const SUBTITLE_SEPARATORS: [char; 10] = ['～', '〜', '~', '―', '—', '–', '－', ':', '：', '|'];
    let title = title.trim();
    let prefix = title
        .char_indices()
        .find(|(_, character)| SUBTITLE_SEPARATORS.contains(character))
        .map(|(index, _)| title[..index].trim());
    prefix
        .filter(|prefix| {
            prefix
                .chars()
                .filter(|character| !character.is_whitespace())
                .count()
                >= 4
        })
        .unwrap_or(title)
}

fn exact_title_result(body: &str, query: &str) -> Result<String, ExternalSearchProviderError> {
    let document = Html::parse_document(body);
    let item_selector = Selector::parse("li[data-list_item_product_id]")
        .expect("valid DLsite result item selector");
    let title_selector =
        Selector::parse(".work_name a").expect("valid DLsite result title selector");
    let normalized_query = normalize_title(query);
    let mut matching = HashSet::new();
    for item in document.select(&item_selector) {
        let Some(rj) = item
            .value()
            .attr("data-list_item_product_id")
            .filter(|rj| valid_rj(rj))
        else {
            continue;
        };
        let Some(title_element) = item.select(&title_selector).next() else {
            continue;
        };
        let title = title_element
            .value()
            .attr("title")
            .map(str::to_owned)
            .unwrap_or_else(|| title_element.text().collect::<String>());
        if normalize_title(&title) == normalized_query {
            matching.insert(rj.to_owned());
        }
    }
    match matching.len() {
        1 => Ok(matching.into_iter().next().expect("one exact title result")),
        0 => Err(provider_error(
            ExternalSearchErrorKind::NoMatch,
            format!("DLsite 搜尋結果沒有與書名「{query}」完全相符的作品"),
        )),
        count => Err(provider_error(
            ExternalSearchErrorKind::NoMatch,
            format!("DLsite 搜尋結果有 {count} 筆與書名「{query}」完全相符，拒絕依排名選擇"),
        )),
    }
}

fn normalize_title(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn valid_rj(value: &str) -> bool {
    value.strip_prefix("RJ").is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn exact_product(body: &str, rj: &str) -> Result<Map<String, Value>, ExternalSearchProviderError> {
    let root: Value = serde_json::from_str(body).map_err(|error| {
        provider_error(
            ExternalSearchErrorKind::InvalidResponse,
            format!("DLsite product JSON 無法解讀：{error}"),
        )
    })?;
    let products = root.as_array().ok_or_else(|| {
        provider_error(
            ExternalSearchErrorKind::InvalidResponse,
            "DLsite product JSON 根結構不是陣列".to_owned(),
        )
    })?;
    if products.is_empty() {
        return Err(provider_error(
            ExternalSearchErrorKind::NoMatch,
            format!("DLsite 沒有回傳產品 {rj}"),
        ));
    }
    let mut matching = products.iter().filter(|product| {
        product.as_object().is_some_and(|product| {
            ["workno", "product_id"]
                .into_iter()
                .filter_map(|key| product.get(key).and_then(Value::as_str))
                .any(|value| value == rj)
        })
    });
    let Some(product) = matching.next() else {
        return Err(provider_error(
            ExternalSearchErrorKind::NoMatch,
            format!("DLsite response 沒有與 {rj} 完全相同的產品識別碼"),
        ));
    };
    if matching.next().is_some() {
        return Err(provider_error(
            ExternalSearchErrorKind::InvalidResponse,
            format!("DLsite response 對 {rj} 回傳多筆完全匹配產品"),
        ));
    }
    let product = product
        .as_object()
        .expect("matching products are JSON objects");
    let response_ids = ["workno", "product_id"]
        .into_iter()
        .filter_map(|key| product.get(key))
        .filter(|value| !value.is_null())
        .collect::<Vec<_>>();
    if response_ids.is_empty() || response_ids.iter().any(|value| value.as_str() != Some(rj)) {
        return Err(provider_error(
            ExternalSearchErrorKind::InvalidResponse,
            format!("DLsite response 的產品識別碼互相衝突：{rj}"),
        ));
    }
    Ok(product.clone())
}

fn map_product(
    request: &ExternalSearchRequest,
    rj: &str,
    product: &Map<String, Value>,
    lookup: &LookupMatch,
) -> ExternalSearchProviderResponse {
    let source_reference = product_reference(product, rj);
    let mut candidates = Vec::new();
    let mut issues = Vec::new();

    if request.fields.contains(&MetadataField::Title) {
        map_text_field(
            request,
            product,
            "work_name",
            MetadataField::Title,
            rj,
            lookup,
            &source_reference,
            &mut candidates,
            &mut issues,
        );
    }
    if request.fields.contains(&MetadataField::Circle) {
        map_text_field(
            request,
            product,
            "maker_name",
            MetadataField::Circle,
            rj,
            lookup,
            &source_reference,
            &mut candidates,
            &mut issues,
        );
    }
    if request.fields.contains(&MetadataField::Authors) {
        match extract_authors(product) {
            Extracted::Missing => {}
            Extracted::Invalid(message) => {
                issues.push(field_issue(MetadataField::Authors, message));
            }
            Extracted::Value(authors) => candidates.push(candidate(
                request,
                MetadataField::Authors,
                MetadataValue::Authors(authors),
                rj,
                lookup,
                "author/authors/creaters.created_by",
                &source_reference,
                0.96,
            )),
        }
    }
    if request.fields.contains(&MetadataField::Event) {
        match extract_event(product) {
            Extracted::Missing => candidates.push(candidate(
                request,
                MetadataField::Event,
                MetadataValue::Text("DL".to_owned()),
                rj,
                lookup,
                "matched_dlsite_product_without_event_option",
                &source_reference,
                1.0,
            )),
            Extracted::Invalid(message) => {
                issues.push(field_issue(MetadataField::Event, message));
            }
            Extracted::Value(event) => candidates.push(candidate(
                request,
                MetadataField::Event,
                MetadataValue::Text(event),
                rj,
                lookup,
                "work_options[event]",
                &source_reference,
                0.98,
            )),
        }
    }
    if request.fields.contains(&MetadataField::Parody) {
        match extract_original(product, lookup) {
            Extracted::Missing => {}
            Extracted::Invalid(message) => {
                issues.push(field_issue(MetadataField::Parody, message));
            }
            Extracted::Value(parody) => candidates.push(candidate(
                request,
                MetadataField::Parody,
                MetadataValue::Parody(parody),
                rj,
                lookup,
                "work_options[ORW]",
                &source_reference,
                0.99,
            )),
        }
    }
    for field in [MetadataField::Classification, MetadataField::IsDl] {
        if request.fields.contains(&field) {
            issues.push(ExternalSearchProviderIssue {
                field: Some(field),
                kind: ExternalSearchErrorKind::Unsupported,
                message: format!("DLsite provider 第一階段不映射 {}", field.as_str()),
            });
        }
    }

    ExternalSearchProviderResponse {
        candidates,
        tags: Vec::new(),
        issues,
    }
}

#[allow(clippy::too_many_arguments)]
fn map_text_field(
    request: &ExternalSearchRequest,
    product: &Map<String, Value>,
    source_field: &str,
    field: MetadataField,
    rj: &str,
    lookup: &LookupMatch,
    source_reference: &str,
    candidates: &mut Vec<ExternalMetadataCandidate>,
    issues: &mut Vec<ExternalSearchProviderIssue>,
) {
    match extract_text(product.get(source_field)) {
        Extracted::Missing => {}
        Extracted::Invalid(message) => issues.push(field_issue(field, message)),
        Extracted::Value(value) => candidates.push(candidate(
            request,
            field,
            MetadataValue::Text(value),
            rj,
            lookup,
            source_field,
            source_reference,
            0.99,
        )),
    }
}

enum Extracted<T> {
    Missing,
    Value(T),
    Invalid(String),
}

fn extract_text(value: Option<&Value>) -> Extracted<String> {
    match value {
        None | Some(Value::Null) => Extracted::Missing,
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                Extracted::Missing
            } else {
                Extracted::Value(value.to_owned())
            }
        }
        Some(_) => Extracted::Invalid("DLsite 文字欄位型別無效".to_owned()),
    }
}

fn extract_authors(product: &Map<String, Value>) -> Extracted<Authors> {
    let mut names = Vec::new();
    let mut invalid = false;
    for key in ["author", "authors"] {
        match extract_named_array(product.get(key), "author_name") {
            Extracted::Missing => {}
            Extracted::Invalid(_) => invalid = true,
            Extracted::Value(values) => extend_unique(&mut names, values),
        }
    }
    if names.is_empty() && !invalid {
        let created_by = product
            .get("creaters")
            .and_then(Value::as_object)
            .and_then(|creators| creators.get("created_by"));
        match extract_named_array(created_by, "name") {
            Extracted::Missing => {}
            Extracted::Invalid(_) => invalid = true,
            Extracted::Value(values) => extend_unique(&mut names, values),
        }
    }
    if invalid {
        return Extracted::Invalid("DLsite 作者欄位具有無法解讀的型別或項目".to_owned());
    }
    if names.is_empty() {
        Extracted::Missing
    } else {
        Extracted::Value(Authors {
            raw: Some(names.join(", ")),
            values: names,
        })
    }
}

fn extract_named_array(value: Option<&Value>, name_key: &str) -> Extracted<Vec<String>> {
    let Some(value) = value else {
        return Extracted::Missing;
    };
    if value.is_null() {
        return Extracted::Missing;
    }
    let Some(items) = value.as_array() else {
        return Extracted::Invalid("DLsite 人員欄位不是陣列".to_owned());
    };
    let mut names = Vec::new();
    for item in items {
        let Some(name) = item
            .as_object()
            .and_then(|item| item.get(name_key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            return Extracted::Invalid("DLsite 人員項目缺少有效名稱".to_owned());
        };
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_owned());
        }
    }
    if names.is_empty() {
        Extracted::Missing
    } else {
        Extracted::Value(names)
    }
}

fn extract_event(product: &Map<String, Value>) -> Extracted<String> {
    let Some(options) = product.get("work_options") else {
        return Extracted::Missing;
    };
    if options.is_null() {
        return Extracted::Missing;
    }
    if options.as_array().is_some_and(Vec::is_empty) {
        return Extracted::Missing;
    }
    let Some(options) = options.as_object() else {
        return Extracted::Invalid("DLsite work_options 不是 object".to_owned());
    };
    let mut events = Vec::new();
    for (key, option) in options {
        let Some(option) = option.as_object() else {
            continue;
        };
        if option.get("category").and_then(Value::as_str) != Some("event") {
            continue;
        }
        let value = option
            .get("value")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(key);
        if !events.iter().any(|event| event == value) {
            events.push(value.to_owned());
        }
    }
    match events.len() {
        0 => Extracted::Missing,
        1 => Extracted::Value(events.remove(0)),
        count => Extracted::Invalid(format!("DLsite 回傳 {count} 個不同場次，拒絕自動選擇")),
    }
}

fn extract_original(product: &Map<String, Value>, lookup: &LookupMatch) -> Extracted<Parody> {
    let Some(options) = product.get("work_options") else {
        return Extracted::Missing;
    };
    if options.is_null() {
        return Extracted::Missing;
    }
    if options.as_array().is_some_and(Vec::is_empty) {
        return Extracted::Missing;
    }
    let Some(options) = options.as_object() else {
        return Extracted::Invalid("DLsite work_options 不是 object".to_owned());
    };
    let original = options.iter().find(|(key, option)| {
        key.as_str() == "ORW"
            || option
                .as_object()
                .and_then(|option| option.get("value"))
                .and_then(Value::as_str)
                == Some("ORW")
    });
    let Some((_key, original)) = original else {
        return Extracted::Missing;
    };
    let Some(original) = original.as_object() else {
        return Extracted::Invalid("DLsite Original Work option 型別無效".to_owned());
    };
    let raw = original
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("オリジナル作品");
    Extracted::Value(Parody {
        raw: raw.to_owned(),
        canonical: "オリジナル".to_owned(),
        evidence: match lookup {
            LookupMatch::ExactRj { .. } => "dlsite_exact_rj:work_options:ORW",
            LookupMatch::ExactTitle { .. } => "dlsite_exact_title:work_options:ORW",
        }
        .to_owned(),
    })
}

fn extend_unique(target: &mut Vec<String>, values: Vec<String>) {
    for value in values {
        if !target.contains(&value) {
            target.push(value);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn candidate(
    request: &ExternalSearchRequest,
    field: MetadataField,
    value: MetadataValue,
    rj: &str,
    lookup: &LookupMatch,
    source_field: &str,
    source_reference: &str,
    rule_certainty: f64,
) -> ExternalMetadataCandidate {
    let confidence = match lookup {
        LookupMatch::ExactRj { .. } => ConfidenceEvidence {
            total: 0.98,
            source_reliability: 0.98,
            identifier_match: 1.0,
            string_similarity: string_similarity_hint(request, field, &value),
            rule_certainty,
            reliable_identifier_exact_match: true,
            reason: format!(
                "DLsite {rj} 與 response 產品識別碼完全匹配；欄位 {source_field} 通過驗證"
            ),
        },
        LookupMatch::ExactTitle { query, .. } => ConfidenceEvidence {
            total: 0.85,
            source_reliability: 0.95,
            identifier_match: 0.0,
            string_similarity: 1.0,
            rule_certainty,
            reliable_identifier_exact_match: false,
            reason: format!(
                "辨識書名「{query}」與 DLsite 搜尋結果唯一完全相符，取得 {rj}；欄位 {source_field} 通過驗證，但沒有可靠識別碼完全匹配"
            ),
        },
    };
    ExternalMetadataCandidate {
        field,
        confidence,
        value,
        source_reference: source_reference.to_owned(),
    }
}

fn string_similarity_hint(
    request: &ExternalSearchRequest,
    field: MetadataField,
    value: &MetadataValue,
) -> f64 {
    let matches = match (field, value) {
        (MetadataField::Title, MetadataValue::Text(value)) => {
            request.collection.title.as_deref() == Some(value)
        }
        (MetadataField::Event, MetadataValue::Text(value)) => {
            request.collection.event.as_deref() == Some(value)
        }
        (MetadataField::Circle, MetadataValue::Text(value)) => {
            request.collection.circle.as_deref() == Some(value)
        }
        (MetadataField::Authors, MetadataValue::Authors(value)) => {
            request.collection.authors == value.values
        }
        (MetadataField::Parody, MetadataValue::Parody(value)) => {
            request.collection.parody.as_deref() == Some(value.canonical.as_str())
        }
        _ => false,
    };
    if matches { 1.0 } else { 0.5 }
}

fn product_reference(product: &Map<String, Value>, rj: &str) -> String {
    let site_id = product
        .get("site_id")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
        .unwrap_or("maniax");
    format!("{PRODUCT_PAGE_BASE}/{site_id}/work/=/product_id/{rj}.html")
}

fn field_issue(field: MetadataField, message: String) -> ExternalSearchProviderIssue {
    ExternalSearchProviderIssue {
        field: Some(field),
        kind: ExternalSearchErrorKind::InvalidResponse,
        message,
    }
}

fn provider_error(kind: ExternalSearchErrorKind, message: String) -> ExternalSearchProviderError {
    ExternalSearchProviderError { kind, message }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use doujin_app::external_search::{ExternalMetadataProvider, ExternalSearchRequest};
    use doujin_parser::domain::Identifier;
    use doujin_storage::collections::{CollectionSnapshot, MediaKind};
    use doujin_storage::jobs::ExternalSearchErrorKind;
    use doujin_storage::metadata::{MetadataField, MetadataValue};
    use serde_json::Value;

    use super::{
        DEFAULT_MIN_REQUEST_INTERVAL, DlsiteExactProvider, DlsiteHttpClient, DlsiteHttpError,
        DlsiteHttpResponse, title_search_url,
    };

    const PRODUCT: &str = include_str!("../tests/fixtures/product.json");
    const TITLE_RESULTS: &str = r#"
        <ul id="search_result_img_box">
          <li data-list_item_product_id="RJ999999">
            <dd class="work_name"><a title="Different title">Different title</a></dd>
          </li>
          <li data-list_item_product_id="RJ123456">
            <dd class="work_name"><a title="Official title">Official title</a></dd>
          </li>
        </ul>
    "#;

    struct FakeClient {
        responses: Mutex<VecDeque<Result<DlsiteHttpResponse, DlsiteHttpError>>>,
        requests: Mutex<Vec<String>>,
    }

    impl FakeClient {
        fn responding(response: DlsiteHttpResponse) -> Self {
            Self::responding_many([response])
        }

        fn responding_many(responses: impl IntoIterator<Item = DlsiteHttpResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(Ok).collect()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from([Err(DlsiteHttpError {
                    message: message.to_owned(),
                })])),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn request_count(&self) -> usize {
            self.requests.lock().expect("requests").len()
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().expect("requests").clone()
        }

        fn next_response(&self) -> Result<DlsiteHttpResponse, DlsiteHttpError> {
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .expect("scripted response")
        }
    }

    impl DlsiteHttpClient for &FakeClient {
        fn fetch_product(&self, rj: &str) -> Result<DlsiteHttpResponse, DlsiteHttpError> {
            self.requests
                .lock()
                .expect("requests")
                .push(format!("product:{rj}"));
            self.next_response()
        }

        fn search_title(&self, title: &str) -> Result<DlsiteHttpResponse, DlsiteHttpError> {
            self.requests
                .lock()
                .expect("requests")
                .push(format!("title:{title}"));
            self.next_response()
        }
    }

    fn request(identifiers: &[&str], fields: Vec<MetadataField>) -> ExternalSearchRequest {
        request_with_title(identifiers, Some("filename title"), fields)
    }

    fn request_with_title(
        identifiers: &[&str],
        title: Option<&str>,
        fields: Vec<MetadataField>,
    ) -> ExternalSearchRequest {
        ExternalSearchRequest {
            job_id: 1,
            collection: CollectionSnapshot {
                id: 1,
                path: PathBuf::from("test.zip"),
                filename: "test.zip".to_owned(),
                media_kind: MediaKind::Zip,
                root: None,
                title: title.map(str::to_owned),
                event: None,
                circle: Some("filename circle".to_owned()),
                authors: Vec::new(),
                parody: None,
                parody_raw: None,
                classification_top: Some("同人誌".to_owned()),
                classification_subcategory: None,
                is_dl: Some(false),
                tags: Vec::new(),
                created_at: "2026-08-12T00:00:00Z".to_owned(),
                updated_at: "2026-08-12T00:00:00Z".to_owned(),
            },
            identifiers: identifiers
                .iter()
                .map(|value| Identifier {
                    scheme: "RJ".to_owned(),
                    value: (*value).to_owned(),
                    raw: format!("[{value}]"),
                })
                .collect(),
            fields,
        }
    }

    fn response(status: u16, body: &str) -> DlsiteHttpResponse {
        DlsiteHttpResponse {
            status,
            body: body.to_owned(),
        }
    }

    #[test]
    fn title_search_url_keeps_the_required_trailing_slash() {
        let url = title_search_url("とある村の筆下ろし事情").expect("title search URL");

        assert_eq!(
            "https://www.dlsite.com/maniax/fsr/=/keyword/%E3%81%A8%E3%81%82%E3%82%8B%E6%9D%91%E3%81%AE%E7%AD%86%E4%B8%8B%E3%82%8D%E3%81%97%E4%BA%8B%E6%83%85/",
            url.as_str()
        );
    }

    #[test]
    fn exact_product_maps_only_directly_supported_fields() {
        let client = FakeClient::responding(response(200, PRODUCT));
        let provider = DlsiteExactProvider::with_client(&client);
        let result = provider
            .search(&request(&["RJ123456"], MetadataField::ALL.to_vec()))
            .expect("exact response");

        assert_eq!(1, client.request_count());
        assert_eq!(5, result.candidates.len());
        assert_eq!(2, result.issues.len());
        assert_eq!(Some(MetadataField::Classification), result.issues[0].field);
        assert_eq!(ExternalSearchErrorKind::Unsupported, result.issues[0].kind);
        assert_eq!(Some(MetadataField::IsDl), result.issues[1].field);
        assert!(result.candidates.iter().all(|candidate| {
            candidate.confidence.total >= 0.95
                && candidate.confidence.reliable_identifier_exact_match
                && candidate.source_reference
                    == "https://www.dlsite.com/maniax/work/=/product_id/RJ123456.html"
        }));
        assert!(result.candidates.iter().any(|candidate| {
            candidate.field == MetadataField::Title
                && candidate.value == MetadataValue::Text("Official title".to_owned())
        }));
        assert!(result.candidates.iter().any(|candidate| {
            candidate.field == MetadataField::Circle
                && candidate.value == MetadataValue::Text("Official Circle".to_owned())
        }));
        assert!(result.candidates.iter().any(|candidate| {
            candidate.field == MetadataField::Event
                && candidate.value == MetadataValue::Text("C104".to_owned())
        }));
        let authors = result
            .candidates
            .iter()
            .find(|candidate| candidate.field == MetadataField::Authors)
            .expect("authors");
        let MetadataValue::Authors(authors) = &authors.value else {
            panic!("authors value type");
        };
        assert_eq!(["Author A", "Author B"], authors.values.as_slice());
        let parody = result
            .candidates
            .iter()
            .find(|candidate| candidate.field == MetadataField::Parody)
            .expect("parody");
        let MetadataValue::Parody(parody) = &parody.value else {
            panic!("parody value type");
        };
        assert_eq!("オリジナル", parody.canonical);
    }

    #[test]
    fn genre_never_becomes_parody_without_original_option() {
        let mut product: Value = serde_json::from_str(PRODUCT).expect("product fixture");
        product[0]["work_options"]
            .as_object_mut()
            .expect("work options")
            .remove("ORW");
        let body = serde_json::to_string(&product).expect("product response");
        let client = FakeClient::responding(response(200, &body));
        let provider = DlsiteExactProvider::with_client(&client);
        let result = provider
            .search(&request(&["RJ123456"], vec![MetadataField::Parody]))
            .expect("response without original option");

        assert!(result.candidates.is_empty());
        assert!(result.issues.is_empty());
    }

    #[test]
    fn matched_dlsite_product_without_event_option_uses_dl_event_fallback() {
        let mut product: Value = serde_json::from_str(PRODUCT).expect("product fixture");
        product[0]["work_options"]
            .as_object_mut()
            .expect("work options")
            .remove("C104");
        let body = serde_json::to_string(&product).expect("product response");
        let client =
            FakeClient::responding_many([response(200, TITLE_RESULTS), response(200, &body)]);
        let provider = DlsiteExactProvider::with_client(&client);
        let result = provider
            .search(&request_with_title(
                &[],
                Some("Official title"),
                vec![MetadataField::Event],
            ))
            .expect("title fallback response");

        assert_eq!(1, result.candidates.len());
        let event = &result.candidates[0];
        assert_eq!(MetadataField::Event, event.field);
        assert_eq!(MetadataValue::Text("DL".to_owned()), event.value);
        assert_eq!(0.85, event.confidence.total);
        assert!(!event.confidence.reliable_identifier_exact_match);
        assert!(
            event
                .confidence
                .reason
                .contains("matched_dlsite_product_without_event_option")
        );
        assert_eq!(
            "https://www.dlsite.com/maniax/work/=/product_id/RJ123456.html",
            event.source_reference
        );
    }

    #[test]
    fn empty_work_options_array_means_no_event_or_original_options() {
        let mut product: Value = serde_json::from_str(PRODUCT).expect("product fixture");
        product[0]["work_options"] = serde_json::json!([]);
        let body = serde_json::to_string(&product).expect("product response");
        let client =
            FakeClient::responding_many([response(200, TITLE_RESULTS), response(200, &body)]);
        let provider = DlsiteExactProvider::with_client(&client);
        let result = provider
            .search(&request_with_title(
                &[],
                Some("Official title"),
                vec![MetadataField::Event, MetadataField::Parody],
            ))
            .expect("empty work options response");

        assert!(result.issues.is_empty());
        assert_eq!(1, result.candidates.len());
        assert_eq!(MetadataField::Event, result.candidates[0].field);
        assert_eq!(
            MetadataValue::Text("DL".to_owned()),
            result.candidates[0].value
        );
    }

    #[test]
    fn recognized_title_falls_back_to_one_normalized_exact_search_result() {
        let client =
            FakeClient::responding_many([response(200, TITLE_RESULTS), response(200, PRODUCT)]);
        let provider = DlsiteExactProvider::with_client(&client);
        let result = provider
            .search(&request_with_title(
                &[],
                Some("  Official   title "),
                MetadataField::ALL.to_vec(),
            ))
            .expect("title fallback response");

        assert_eq!(
            [
                "title:Official   title".to_owned(),
                "product:RJ123456".to_owned(),
            ],
            client.requests().as_slice()
        );
        assert_eq!(5, result.candidates.len());
        assert!(result.candidates.iter().all(|candidate| {
            candidate.confidence.total >= 0.75
                && candidate.confidence.total < 0.95
                && !candidate.confidence.reliable_identifier_exact_match
                && candidate.confidence.identifier_match == 0.0
                && candidate
                    .confidence
                    .reason
                    .contains("沒有可靠識別碼完全匹配")
        }));
    }

    #[test]
    fn subtitle_separator_uses_stable_title_core_but_matches_the_complete_title() {
        let long_title = "ママとられ2～恥辱に堕ちる冒険者母子～";
        let search_result = format!(
            r#"<li data-list_item_product_id="RJ123456"><dd class="work_name"><a title="{long_title}">{long_title}</a></dd></li>"#
        );
        let client =
            FakeClient::responding_many([response(200, &search_result), response(200, PRODUCT)]);
        let provider = DlsiteExactProvider::with_client(&client);
        let result = provider
            .search(&request_with_title(
                &[],
                Some(long_title),
                vec![MetadataField::Parody],
            ))
            .expect("stable title core response");

        assert_eq!(
            ["title:ママとられ2", "product:RJ123456"],
            client.requests().as_slice()
        );
        assert_eq!(1, result.candidates.len());
        assert!(
            !result.candidates[0]
                .confidence
                .reliable_identifier_exact_match
        );
    }

    #[test]
    fn missing_title_and_ambiguous_rj_never_send_http_request() {
        let client = FakeClient::responding(response(200, PRODUCT));
        let provider = DlsiteExactProvider::with_client(&client);

        let missing = provider
            .search(&request_with_title(
                &[],
                Some("   "),
                vec![MetadataField::Title],
            ))
            .expect_err("missing RJ and title");
        assert_eq!(ExternalSearchErrorKind::Unsupported, missing.kind);
        let ambiguous = provider
            .search(&request(
                &["RJ123456", "RJ654321"],
                vec![MetadataField::Title],
            ))
            .expect_err("ambiguous RJ");
        assert_eq!(ExternalSearchErrorKind::Unsupported, ambiguous.kind);
        assert_eq!(0, client.request_count());
    }

    #[test]
    fn duplicate_exact_title_results_are_not_selected_by_rank() {
        let duplicated = TITLE_RESULTS.replace(
            "Different title\">Different title",
            "Official title\">Official title",
        );
        let client = FakeClient::responding(response(200, &duplicated));
        let provider = DlsiteExactProvider::with_client(&client);
        let error = provider
            .search(&request_with_title(
                &[],
                Some("Official title"),
                vec![MetadataField::Parody],
            ))
            .expect_err("ambiguous title results");

        assert_eq!(ExternalSearchErrorKind::NoMatch, error.kind);
        assert!(error.message.contains("2 筆"));
        assert_eq!(["title:Official title"], client.requests().as_slice());
    }

    #[test]
    fn title_search_failures_keep_typed_error_kinds() {
        let network_client = FakeClient::failing("timeout");
        let provider = DlsiteExactProvider::with_client(&network_client);
        assert_eq!(
            ExternalSearchErrorKind::Network,
            provider
                .search(&request_with_title(
                    &[],
                    Some("Official title"),
                    vec![MetadataField::Title],
                ))
                .expect_err("title search network error")
                .kind
        );

        for (status, kind) in [
            (404, ExternalSearchErrorKind::NoMatch),
            (429, ExternalSearchErrorKind::RateLimited),
            (503, ExternalSearchErrorKind::ProviderUnavailable),
            (403, ExternalSearchErrorKind::Unsupported),
            (302, ExternalSearchErrorKind::InvalidResponse),
        ] {
            let client = FakeClient::responding(response(status, ""));
            let provider = DlsiteExactProvider::with_client(&client);
            assert_eq!(
                kind,
                provider
                    .search(&request_with_title(
                        &[],
                        Some("Official title"),
                        vec![MetadataField::Title],
                    ))
                    .expect_err("title search HTTP error")
                    .kind
            );
        }
    }

    #[test]
    fn malformed_optional_authors_preserve_valid_title_and_circle() {
        let mut product: Value = serde_json::from_str(PRODUCT).expect("product fixture");
        product[0]["authors"] = Value::String("invalid".to_owned());
        let body = serde_json::to_string(&product).expect("product response");
        let client = FakeClient::responding(response(200, &body));
        let provider = DlsiteExactProvider::with_client(&client);
        let result = provider
            .search(&request(
                &["RJ123456"],
                vec![
                    MetadataField::Title,
                    MetadataField::Circle,
                    MetadataField::Authors,
                ],
            ))
            .expect("partial field response");

        assert_eq!(2, result.candidates.len());
        assert_eq!(1, result.issues.len());
        assert_eq!(Some(MetadataField::Authors), result.issues[0].field);
        assert_eq!(
            ExternalSearchErrorKind::InvalidResponse,
            result.issues[0].kind
        );
    }

    #[test]
    fn transport_and_http_failures_have_typed_error_kinds() {
        let network_client = FakeClient::failing("timeout");
        let provider = DlsiteExactProvider::with_client(&network_client);
        assert_eq!(
            ExternalSearchErrorKind::Network,
            provider
                .search(&request(&["RJ123456"], vec![MetadataField::Title]))
                .expect_err("network")
                .kind
        );

        for (status, kind) in [
            (404, ExternalSearchErrorKind::NoMatch),
            (429, ExternalSearchErrorKind::RateLimited),
            (503, ExternalSearchErrorKind::ProviderUnavailable),
            (403, ExternalSearchErrorKind::Unsupported),
            (302, ExternalSearchErrorKind::InvalidResponse),
        ] {
            let client = FakeClient::responding(response(status, ""));
            let provider = DlsiteExactProvider::with_client(&client);
            assert_eq!(
                kind,
                provider
                    .search(&request(&["RJ123456"], vec![MetadataField::Title]))
                    .expect_err("HTTP error")
                    .kind
            );
        }
    }

    #[test]
    fn root_shape_empty_array_and_id_mismatch_are_rejected() {
        for (body, kind) in [
            ("{}", ExternalSearchErrorKind::InvalidResponse),
            ("[]", ExternalSearchErrorKind::NoMatch),
            (
                r#"[{"workno":"RJ999999","product_id":"RJ999999"}]"#,
                ExternalSearchErrorKind::NoMatch,
            ),
        ] {
            let client = FakeClient::responding(response(200, body));
            let provider = DlsiteExactProvider::with_client(&client);
            assert_eq!(
                kind,
                provider
                    .search(&request(&["RJ123456"], vec![MetadataField::Title]))
                    .expect_err("invalid exact response")
                    .kind
            );
        }
    }

    #[test]
    fn production_default_uses_ten_second_minimum_interval() {
        assert_eq!(
            std::time::Duration::from_secs(10),
            DEFAULT_MIN_REQUEST_INTERVAL
        );
    }
}
