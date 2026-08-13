use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use doujin_app::{ApplicationService, ApplicationSettingsOverrides};
use doujin_files::{CollectionLauncher, RecycleBin};
use doujin_http::{
    SharedApplication, bind_loopback, serve_shared_with_shutdown, share_application,
    validate_loopback_address,
};
use doujin_scanner::{ScanRoot, SourceKind};
use doujin_storage::CatalogRepository;
use doujin_storage::metadata::{
    ConfidenceEvidence, ExternalCandidate, ExternalCandidateOutcome, MetadataField, MetadataValue,
};
use doujin_storage::thumbnails::{ThumbnailErrorKind, ThumbnailStatus};
use doujin_thumbnails::{
    ThumbnailConfig, ThumbnailError, ThumbnailGenerationSuccess, transparent_placeholder_webp,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy)]
struct NoopRecycleBin;

impl RecycleBin for NoopRecycleBin {
    fn recycle(&self, _path: &Path) -> Result<(), String> {
        Ok(())
    }
}

struct FakeRecycleBin {
    directory: PathBuf,
}

impl RecycleBin for FakeRecycleBin {
    fn recycle(&self, path: &Path) -> Result<(), String> {
        let filename = path
            .file_name()
            .ok_or_else(|| "soft delete path 缺少檔名".to_owned())?;
        fs::rename(path, self.directory.join(filename)).map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchCall {
    reader: Option<PathBuf>,
    path: PathBuf,
}

#[derive(Clone)]
struct RecordingLauncher {
    calls: Arc<Mutex<Vec<LaunchCall>>>,
    fail: bool,
}

impl RecordingLauncher {
    fn new(fail: bool) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            fail,
        }
    }

    fn calls(&self) -> Vec<LaunchCall> {
        self.calls.lock().expect("launcher calls").clone()
    }
}

impl CollectionLauncher for RecordingLauncher {
    fn open_default(&self, path: &Path) -> io::Result<()> {
        self.record(None, path)
    }

    fn open_with_reader(&self, reader: &Path, path: &Path) -> io::Result<()> {
        self.record(Some(reader.to_owned()), path)
    }
}

impl RecordingLauncher {
    fn record(&self, reader: Option<PathBuf>, path: &Path) -> io::Result<()> {
        if self.fail {
            return Err(io::Error::other("simulated launcher failure"));
        }
        self.calls.lock().expect("launcher calls").push(LaunchCall {
            reader,
            path: path.to_owned(),
        });
        Ok(())
    }
}

fn confidence(total: f64) -> ConfidenceEvidence {
    ConfidenceEvidence {
        total,
        source_reliability: 0.9,
        identifier_match: 0.6,
        string_similarity: 0.8,
        rule_certainty: 0.7,
        reliable_identifier_exact_match: false,
        reason: "HTTP metadata history test".to_owned(),
    }
}

struct TestTree {
    path: PathBuf,
}

impl TestTree {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "doujin-http-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test tree");
        Self { path }
    }

    fn library(&self) -> PathBuf {
        self.root("library")
    }

    fn zip(&self, filename: &str) {
        self.zip_in("library", filename);
    }

    fn root(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    fn zip_in(&self, root: &str, filename: &str) {
        let path = self.root(root).join(filename);
        fs::create_dir_all(path.parent().expect("zip parent")).expect("create library");
        fs::write(path, b"zip placeholder").expect("create zip");
    }
}

impl Drop for TestTree {
    fn drop(&mut self) {
        if self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("doujin-http-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

struct RunningServer {
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<(), doujin_http::ServerError>>,
}

impl RunningServer {
    async fn start<R>(application: ApplicationService<R>) -> Self
    where
        R: RecycleBin + Send + 'static,
    {
        Self::start_shared(share_application(application)).await
    }

    async fn start_shared<R>(application: SharedApplication<R>) -> Self
    where
        R: RecycleBin + Send + 'static,
    {
        let listener = bind_loopback(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind loopback");
        let address = listener.local_addr().expect("listener address");
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(serve_shared_with_shutdown(listener, application, async {
            let _ = receiver.await;
        }));
        Self {
            address,
            shutdown: Some(shutdown),
            task,
        }
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        extra_headers: &[(&str, &str)],
    ) -> HttpResult {
        self.request_with_body(method, path, extra_headers, "")
            .await
    }

    async fn request_json(&self, method: &str, path: &str, body: &Value) -> HttpResult {
        self.request_with_body(
            method,
            path,
            &[("Content-Type", "application/json")],
            &body.to_string(),
        )
        .await
    }

    async fn request_with_body(
        &self,
        method: &str,
        path: &str,
        extra_headers: &[(&str, &str)],
        body: &str,
    ) -> HttpResult {
        let mut stream = tokio::net::TcpStream::connect(self.address)
            .await
            .expect("connect HTTP server");
        let host = extra_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("host"))
            .map(|(_, value)| (*value).to_owned())
            .unwrap_or_else(|| self.address.to_string());
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (name, value) in extra_headers {
            if name.eq_ignore_ascii_case("host") {
                continue;
            }
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write HTTP request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read HTTP response");
        HttpResult::parse(&response)
    }

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task
            .await
            .expect("server task")
            .expect("server result");
    }
}

#[tokio::test]
async fn rust_frontend_is_embedded_with_local_only_assets_and_security_headers() {
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;

    let index = server.request("GET", "/", &[]).await;
    assert_eq!(200, index.status);
    assert_eq!(
        Some("text/html; charset=utf-8"),
        index.header("content-type")
    );
    assert_eq!(Some("no-store"), index.header("cache-control"));
    assert_eq!(Some("nosniff"), index.header("x-content-type-options"));
    assert_eq!(Some("no-referrer"), index.header("referrer-policy"));
    let policy = index
        .header("content-security-policy")
        .expect("content security policy");
    assert!(policy.contains("default-src 'none'"));
    assert!(policy.contains("connect-src 'self'"));
    assert!(policy.contains("frame-ancestors 'none'"));
    let document = String::from_utf8(index.body).expect("UTF-8 frontend document");
    assert!(document.contains("<html lang=\"zh-Hant\">"));
    assert!(document.contains("id=\"main-content\""));
    assert!(document.contains("aria-live=\"polite\""));
    assert!(document.contains("href=\"/assets/app.css?v=44\""));
    assert!(document.contains("src=\"/assets/app.js?v=47\" defer"));
    assert!(document.contains("id=\"library-scroll-sentinel\""));
    assert!(document.contains("id=\"library-load-more\""));
    assert!(document.contains("id=\"library-load-announcer\""));
    assert!(document.contains("id=\"scan-results-dialog\""));
    assert!(document.contains("id=\"thumbnail-cache-preflight-dialog\""));
    assert!(document.contains("id=\"thumbnail-cache-retry-failures\""));
    assert!(document.contains("id=\"edit-root-dialog\""));
    assert!(document.contains("id=\"root-rescan-note\""));
    assert!(document.contains("id=\"viewer-path-override\""));
    assert!(document.contains("id=\"library-empty-heading\""));
    assert!(document.contains("id=\"library-empty-primary\""));
    assert!(document.contains("id=\"library-sort\""));
    assert!(document.contains("最近修改"));
    assert!(document.contains("全選已載入"));
    assert!(document.contains("新載入結果不會自動加入"));
    assert!(!document.contains("目前頁面選取"));
    assert!(!document.contains("全選本頁"));
    assert!(!document.contains("id=\"previous-page\""));
    assert!(!document.contains("id=\"next-page\""));
    assert!(document.contains("id=\"close-filter-panel\""));
    assert!(document.contains("id=\"header-search-scope\""));
    assert!(document.contains("在目前結果中搜尋"));
    assert!(document.contains("id=\"filter-draft-status\""));
    assert!(document.contains("id=\"discard-filter-dialog\""));
    assert!(document.contains("id=\"batch-progress\""));
    assert!(document.contains("id=\"retry-batch-failures\""));
    assert!(document.contains("id=\"shelf-view\""));
    assert!(document.contains("data-route=\"shelf\""));
    assert!(document.contains("id=\"workbench-view\""));
    assert!(document.contains("id=\"return-to-library-context\""));
    assert!(document.contains("返回原本的藏書位置調整選取"));
    assert!(document.contains("id=\"focus-filter-dialog\""));
    assert!(document.contains("清除篩選並定位"));
    assert!(document.contains("id=\"metadata-evidence\""));
    assert!(document.contains("id=\"mobile-detail-dialog\""));
    assert!(document.contains("id=\"close-mobile-detail\""));
    assert!(document.contains("id=\"activity-trigger\""));
    assert!(document.contains("id=\"activity-panel\""));
    assert!(document.contains("id=\"activity-announcer\""));
    assert!(document.contains("最大原作書架"));
    assert!(document.contains("data-shelf-scroll=\"next\""));
    assert!(document.contains("role=\"combobox\""));
    assert!(document.contains("id=\"filter-tag-chips\""));
    assert!(document.contains("尚無標籤"));
    assert!(document.contains("id=\"external-job-status\""));
    assert!(document.contains("資料品質與來源"));
    assert!(document.contains("id=\"data-quality-summary\""));
    assert!(document.contains("id=\"missing-metadata-actions\""));
    assert!(document.contains("id=\"detail-tag-options\""));
    assert!(document.contains("id=\"batch-tag-options\""));
    assert!(document.contains("id=\"selection-workbench-link\""));
    assert!(document.contains("移動目前查看收藏"));
    assert!(document.contains("切換目前查看收藏的批次選取"));
    assert!(!document.contains(">加標籤</a>"));
    assert!(!document.contains(">寫入手動值</a>"));
    assert!(document.contains("id=\"delete-dialog\""));
    assert!(document.contains("id=\"consolidation-dialog\""));
    assert!(document.contains("移到資源回收桶"));
    assert!(!document.contains("https://"));

    let css = server.request("GET", "/assets/app.css", &[]).await;
    assert_eq!(200, css.status);
    assert_eq!(Some("text/css; charset=utf-8"), css.header("content-type"));
    assert_eq!(Some("no-cache"), css.header("cache-control"));
    let stylesheet = String::from_utf8(css.body).expect("UTF-8 stylesheet");
    assert!(stylesheet.contains("prefers-reduced-motion: reduce"));
    assert!(stylesheet.contains(":focus-visible"));
    assert!(stylesheet.contains("min-width: 320px"));
    assert!(stylesheet.contains("width: 44px;"));
    assert!(stylesheet.contains("height: 44px;"));
    assert!(stylesheet.contains("(pointer: coarse)"));
    assert!(stylesheet.contains("--muted: #786e60;"));
    assert!(stylesheet.contains("outline: 3px solid var(--focus);"));
    assert!(stylesheet.contains(".filter-draft-status"));
    assert!(stylesheet.contains(".batch-progress"));
    assert!(stylesheet.contains(".collection-window-spacer"));
    assert!(stylesheet.contains("contain: layout paint style"));
    assert!(stylesheet.contains(".scan-issue-list"));
    assert!(stylesheet.contains(".long-task-warning"));
    assert!(stylesheet.contains(".root-actions"));
    assert!(stylesheet.contains(".field-override-note"));
    assert!(stylesheet.contains(".empty-state-actions"));
    assert!(stylesheet.contains(".sort-control"));
    assert!(stylesheet.contains(".missing-metadata-actions"));
    assert!(stylesheet.contains(".tag-suggestion-combobox"));
    assert!(!stylesheet.contains("font-size: 0.6875rem;"));
    assert!(!stylesheet.contains("font-size: 0.625rem;"));
    assert!(!stylesheet.contains("font-size: 0.5625rem;"));
    assert!(!stylesheet.contains("color: var(--faint);"));
    assert!(stylesheet.starts_with("/* 1. Tokens */"));
    assert!(stylesheet.contains("/* 10. Responsive, input modality, and reduced motion */"));
    assert!(!stylesheet.contains("UI redesign v19"));
    assert!(!stylesheet.contains(".brand-mark"));

    let javascript = server.request("GET", "/assets/app.js", &[]).await;
    assert_eq!(200, javascript.status);
    assert_eq!(
        Some("text/javascript; charset=utf-8"),
        javascript.header("content-type")
    );
    let script = String::from_utf8(javascript.body).expect("UTF-8 script");
    assert!(script.contains("doujin-library.recent.v1"));
    assert!(script.contains("const RECENT_LIMIT = 20"));
    assert!(script.contains("const PER_PAGE = 48"));
    assert!(script.contains("loadMoreCollections"));
    assert!(script.contains("if (libraryLoadPromise) return libraryLoadPromise"));
    assert!(script.contains("function moveLibraryFocus"));
    assert!(script.contains("button?.scrollIntoView({ block: \"nearest\" })"));
    assert!(script.contains("已顯示全部 ${formatNumber(state.total)} 筆收藏"));
    assert!(script.contains(
        "已載入 ${formatNumber(additions.length)} 筆，尚有 ${formatNumber(remaining)} 筆"
    ));
    assert!(script.contains("rootMargin: \"1200px 0px\""));
    assert!(script.contains("/api/collections"));
    assert!(script.contains("rememberLaunch(state.selected, kind)"));
    assert!(script.contains("applyFilter(filterName, row.name)"));
    assert!(script.contains("/api/file-actions/move"));
    assert!(script.contains("/api/file-actions/delete"));
    assert!(script.contains("/api/tombstone-candidates"));
    assert!(script.contains("executeConsolidation"));
    assert!(script.contains("loadMetadataEvidence"));
    assert!(script.contains("renderDataQualitySummary"));
    assert!(script.contains("refreshActivityCenter"));
    assert!(script.contains("updateShelfScrollControls"));
    assert!(script.contains("decideMetadataAssertion"));
    assert!(script.contains("/metadata/${field}/assertions/${assertion.id}"));
    assert!(script.contains("doujin-library.external-jobs.v1"));
    assert!(script.contains("error.code === \"application_busy\""));
    assert!(script.contains("requestTrackedThumbnail"));
    assert!(script.contains("const THUMBNAIL_REQUEST_CONCURRENCY = 4"));
    assert!(script.contains("ensureThumbnailStatusLabel"));
    assert!(script.contains("drainThumbnailRequestQueue"));
    assert!(script.contains("rootMargin: \"800px 0px\""));
    assert!(script.contains("nextThumbnailRequestEpoch"));
    assert!(script.contains("?priority=${encodeURIComponent(priority)}"));
    assert!(script.contains("const thumbnail = await response.blob()"));
    assert!(script.contains("const readyUrl = await blobAsDataUrl(thumbnail)"));
    assert!(script.contains("x-thumbnail-next-retry-at"));
    assert!(script.contains("restartThumbnailCollection"));
    assert!(script.contains("requestFilterPanelClose({ restoreFocus: true })"));
    assert!(script.contains("updateSelectionCheckbox(selection"));
    assert!(script.contains("從批次選取移除"));
    assert!(script.contains("查看 ${displayTitle(collection)} 詳情"));
    assert!(!script.contains("`選取 ${displayTitle(collection)}`"));
    assert!(script.contains("批次選取 ${formatNumber(state.selectedIds.size)} / 已載入 ${formatNumber(state.items.length)} / 符合 ${formatNumber(state.total)}"));
    assert!(script.contains("function initializeTagSuggestionInputs"));
    assert!(script.contains("使用 ${formatNumber(option.count)} 次"));
    assert!(script.contains("controller.form?.requestSubmit()"));
    assert!(script.contains("function renderMissingMetadataActions"));
    assert!(script.contains("openMetadataDialog(field)"));
    assert!(script.contains("前往工作台處理 ${formatNumber(count)} 筆"));
    assert!(script.contains("function selectionImpactSummary"));
    assert!(script.contains("其餘 ${formatNumber(unaffectedCount)} 筆不受影響"));
    assert!(script.contains("const isCollectionButton"));
    assert!(script.contains("openMobileDetail(button, scrollPosition)"));
    assert!(script.contains("finishMobileDetailClose"));
    assert!(script.contains("--mobile-detail-scroll-offset"));
    assert!(script.contains("dialog[open]:not(#mobile-detail-dialog)"));
    assert!(!script.contains("byId(\"detail-pane\").scrollIntoView"));
    assert!(script.contains("/api/facets?${params}"));
    assert!(script.contains("params.append(name, entry)"));
    assert!(script.contains("aria-activedescendant"));
    assert!(script.contains("function applyFilterDraft"));
    assert!(script.contains("function filterDraftChanged"));
    assert!(script.contains("function applyHeaderSearch"));
    assert!(script.contains("function runBatchOperation"));
    assert!(script.contains("const BATCH_REQUEST_SIZE = 100"));
    assert!(script.contains("const COLLECTION_WINDOW_SIZE = 384"));
    assert!(script.contains("function renderCollectionWindow"));
    assert!(script.contains("function collectionWindowRange"));
    assert!(script.contains("function ensureCollectionMounted"));
    assert!(script.contains("function showScanResults"));
    assert!(script.contains("function openThumbnailCacheFailures"));
    assert!(script.contains("function retryThumbnailCacheFailures"));
    assert!(script.contains("function saveEditedRoot"));
    assert!(script.contains("function reactivateRoot"));
    assert!(script.contains("settingsSnapshot.saved_thumb_size"));
    assert!(script.contains("function resolveLibraryEmptyContext"));
    assert!(script.contains("function renderLibraryEmptyState"));
    assert!(script.contains("function scanEmptyLibrary"));
    assert!(script.contains("openLibraryRootSettings(\"new\")"));
    assert!(script.contains("function changeLibrarySort"));
    assert!(script.contains("sort: state.sort"));
    assert!(script.contains("params.set(\"direction\", direction)"));
    assert!(script.contains("/api/thumbnail-cache-jobs/preflight"));
    assert!(script.contains("/api/scans/latest"));
    assert!(script.contains("if (state.route === \"workbench\") renderWorkbenchSelection()"));
    assert!(!script.contains("document.querySelectorAll(\".collection-item-button\")"));
    assert!(!script.contains("function runSelectedRequests"));
    assert!(script.contains("/api/batch/tags"));
    assert!(script.contains("/api/batch/metadata/${field}"));
    assert!(script.contains("function retryFailedBatch"));
    assert!(script.contains("invalidateDerivedData({ library: true })"));
    assert!(
        script
            .contains("state.route === \"library\" && ui.headerSearchScope.value === \"current\"")
    );
    assert!(script.contains("item.addEventListener(\"click\", () => selectFacetOption"));
    assert!(!script.contains("item.addEventListener(\"pointerdown\""));
    assert!(script.contains("function decodeLibraryParams"));
    assert!(script.contains("function rememberLibraryContext"));
    assert!(script.contains("function returnToLibraryContext"));
    assert!(script.contains("restoreThroughPage: state.libraryRestorePage"));
    assert!(script.contains("if (!preserveSelection) clearSelection()"));
    assert!(script.contains("renderCollections({ deferFocus })"));
    assert!(script.contains("if (deferFocus) resolveLibraryFocus()"));
    assert!(script.contains("/api/collections/${focusId}/locate?${params}"));
    assert!(script.contains("function presentOutOfQueryFocus"));
    assert!(script.contains("await navigateToCollection(collection)"));
    assert!(!script.contains("location.hash = \"library\""));
    assert!(script.contains("function confirmSelectionClear"));
    assert!(script.contains("這會清除目前"));
    assert!(script.contains("event.key === \"Escape\" && !ui.filterPanel.hidden"));
    assert!(script.contains("永久刪除 ${state.selectedIds.size} 筆"));

    let unknown_asset = server.request("GET", "/assets/unknown.css", &[]).await;
    assert_eq!(404, unknown_asset.status);
    assert_eq!("route_not_found", unknown_asset.json["error"]["code"]);
    server.stop().await;
}

#[tokio::test]
async fn library_roots_can_be_registered_listed_deactivated_and_reactivated() {
    let tree = TestTree::new("library-roots");
    fs::create_dir_all(tree.library()).expect("create library");
    let edited_path = tree.path.join("edited-library");
    fs::create_dir_all(&edited_path).expect("create edited library");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;

    let empty = server.request("GET", "/api/library-roots", &[]).await;
    assert_eq!(200, empty.status);
    assert_eq!(0, empty.json["roots"].as_array().expect("roots").len());

    let registered = server
        .request_json(
            "POST",
            "/api/library-roots",
            &serde_json::json!({
                "path": tree.library(),
                "source": "downloads",
                "label": "  下載區  "
            }),
        )
        .await;
    assert_eq!(200, registered.status);
    assert_eq!("downloads", registered.json["source"]);
    assert_eq!("下載區", registered.json["label"]);
    assert_eq!(true, registered.json["active"]);
    let root_id = registered.json["id"].as_i64().expect("root ID");

    let listed = server.request("GET", "/api/library-roots", &[]).await;
    assert_eq!(1, listed.json["roots"].as_array().expect("roots").len());
    assert_eq!(root_id, listed.json["roots"][0]["id"]);

    let edited = server
        .request_json(
            "PATCH",
            &format!("/api/library-roots/{root_id}"),
            &serde_json::json!({
                "path": edited_path,
                "source": "archive",
                "label": "  編輯後典藏區  "
            }),
        )
        .await;
    assert_eq!(200, edited.status);
    assert_eq!(root_id, edited.json["id"]);
    assert_eq!("archive", edited.json["source"]);
    assert_eq!("編輯後典藏區", edited.json["label"]);

    let deactivated = server
        .request("DELETE", &format!("/api/library-roots/{root_id}"), &[])
        .await;
    assert_eq!(200, deactivated.status);
    assert_eq!(false, deactivated.json["active"]);

    let scan = server.request("POST", "/api/scans", &[]).await;
    assert_eq!(200, scan.status);
    assert_eq!(0, scan.json["summary"]["roots"]);
    assert_eq!("no_roots", scan.json["issues"][0]["kind"]);

    let reactivated = server
        .request(
            "POST",
            &format!("/api/library-roots/{root_id}/activate"),
            &[],
        )
        .await;
    assert_eq!(root_id, reactivated.json["id"]);
    assert_eq!("archive", reactivated.json["source"]);
    assert_eq!("編輯後典藏區", reactivated.json["label"]);
    assert_eq!(true, reactivated.json["active"]);

    let listed_again = server.request("GET", "/api/library-roots", &[]).await;
    assert_eq!(
        1,
        listed_again.json["roots"].as_array().expect("roots").len()
    );
    server.stop().await;
}

#[tokio::test]
async fn invalid_library_root_requests_have_structured_json_errors() {
    let tree = TestTree::new("invalid-library-roots");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;

    let relative = server
        .request_json(
            "POST",
            "/api/library-roots",
            &serde_json::json!({
                "path": "relative/path",
                "source": "archive",
                "label": "歸檔區"
            }),
        )
        .await;
    assert_eq!(400, relative.status);
    assert_eq!("invalid_library_root", relative.json["error"]["code"]);

    let missing = server
        .request_json(
            "POST",
            "/api/library-roots",
            &serde_json::json!({
                "path": tree.path.join("missing"),
                "source": "archive",
                "label": "歸檔區"
            }),
        )
        .await;
    assert_eq!(400, missing.status);
    assert_eq!("invalid_library_root", missing.json["error"]["code"]);

    let invalid_source = server
        .request_json(
            "POST",
            "/api/library-roots",
            &serde_json::json!({
                "path": tree.path,
                "source": "other",
                "label": "其他"
            }),
        )
        .await;
    assert_eq!(400, invalid_source.status);
    assert_eq!(
        "invalid_library_root_source",
        invalid_source.json["error"]["code"]
    );

    let malformed = server
        .request_with_body(
            "POST",
            "/api/library-roots",
            &[("Content-Type", "application/json")],
            "{",
        )
        .await;
    assert_eq!(400, malformed.status);
    assert_eq!("invalid_json", malformed.json["error"]["code"]);

    let invalid_id = server
        .request("DELETE", "/api/library-roots/nope", &[])
        .await;
    assert_eq!(400, invalid_id.status);
    assert_eq!("invalid_library_root_id", invalid_id.json["error"]["code"]);

    let unknown = server
        .request("DELETE", "/api/library-roots/999", &[])
        .await;
    assert_eq!(404, unknown.status);
    assert_eq!("library_root_not_found", unknown.json["error"]["code"]);

    let invalid_update = server
        .request_json(
            "PATCH",
            "/api/library-roots/999",
            &serde_json::json!({
                "path": tree.path,
                "source": "archive",
                "label": "不存在"
            }),
        )
        .await;
    assert_eq!(404, invalid_update.status);
    assert_eq!(
        "library_root_not_found",
        invalid_update.json["error"]["code"]
    );
    server.stop().await;
}

#[tokio::test]
async fn collections_support_paging_safe_search_and_detail_over_loopback() {
    let tree = TestTree::new("collections");
    tree.zip("[AlphaCircle (Alice)] First Story.zip");
    tree.zip("[BetaCircle (Bob)] Second Story.zip");
    tree.zip("RJ123456 [GammaCircle] Third Story.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "歸檔區")
        .expect("register root");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;
    let scan = server.request("POST", "/api/scans", &[]).await;
    assert_eq!(200, scan.status);
    assert_eq!(3, scan.json["summary"]["added"]);

    let first_page = server
        .request("GET", "/api/collections?page=1&per_page=2", &[])
        .await;
    assert_eq!(200, first_page.status);
    assert_eq!(3, first_page.json["pagination"]["total"]);
    assert_eq!(2, first_page.json["pagination"]["total_pages"]);
    assert_eq!(2, first_page.json["items"].as_array().expect("items").len());
    let default_first_id = first_page.json["items"][0]["id"]
        .as_i64()
        .expect("default first ID");

    let second_page = server
        .request("GET", "/api/collections?page=2&per_page=2", &[])
        .await;
    assert_eq!(
        1,
        second_page.json["items"].as_array().expect("items").len()
    );
    let locator_id = second_page.json["items"][0]["id"]
        .as_i64()
        .expect("second page collection ID");

    let clamped = server
        .request("GET", "/api/collections?page=0&per_page=999", &[])
        .await;
    assert_eq!(1, clamped.json["pagination"]["page"]);
    assert_eq!(200, clamped.json["pagination"]["per_page"]);

    let unsupported_sort = server
        .request(
            "GET",
            "/api/collections?sort=not_allowed&direction=sideways",
            &[],
        )
        .await;
    assert_eq!(200, unsupported_sort.status);
    assert_eq!(3, unsupported_sort.json["pagination"]["total"]);
    assert_eq!(default_first_id, unsupported_sort.json["items"][0]["id"]);

    let title_ascending = server
        .request(
            "GET",
            "/api/collections?sort=title&direction=asc&per_page=2",
            &[],
        )
        .await;
    assert_eq!(200, title_ascending.status);
    assert_eq!("First Story", title_ascending.json["items"][0]["title"]);
    assert_eq!(
        "RJ123456 [GammaCircle] Third Story",
        title_ascending.json["items"][1]["title"]
    );
    let title_second_page = server
        .request(
            "GET",
            "/api/collections?sort=title&direction=asc&page=2&per_page=2",
            &[],
        )
        .await;
    assert_eq!("Second Story", title_second_page.json["items"][0]["title"]);
    let title_locator_id = title_second_page.json["items"][0]["id"]
        .as_i64()
        .expect("title locator ID");
    let title_located = server
        .request(
            "GET",
            &format!(
                "/api/collections/{title_locator_id}/locate?sort=title&direction=asc&per_page=2"
            ),
            &[],
        )
        .await;
    assert_eq!(3, title_located.json["position"]);
    assert_eq!(2, title_located.json["page"]);

    let title_descending = server
        .request("GET", "/api/collections?sort=title&direction=desc", &[])
        .await;
    assert_eq!("Second Story", title_descending.json["items"][0]["title"]);

    let metadata_search = server
        .request("GET", "/api/collections?q=AlphaCircle", &[])
        .await;
    assert_eq!(200, metadata_search.status);
    assert_eq!(1, metadata_search.json["pagination"]["total"]);
    assert_eq!("AlphaCircle", metadata_search.json["items"][0]["circle"]);
    assert_eq!("Alice", metadata_search.json["items"][0]["authors"][0]);
    assert_eq!(
        "archive",
        metadata_search.json["items"][0]["root"]["source"]
    );
    let collection_id = metadata_search.json["items"][0]["id"]
        .as_i64()
        .expect("collection ID");

    let located = server
        .request(
            "GET",
            &format!("/api/collections/{locator_id}/locate?per_page=2"),
            &[],
        )
        .await;
    assert_eq!(200, located.status);
    assert_eq!("in_query", located.json["status"]);
    assert_eq!(3, located.json["position"]);
    assert_eq!(2, located.json["page"]);
    assert_eq!(locator_id, located.json["collection"]["id"]);

    let outside_query = server
        .request(
            "GET",
            &format!("/api/collections/{locator_id}/locate?q=no-such-title&per_page=2"),
            &[],
        )
        .await;
    assert_eq!(200, outside_query.status);
    assert_eq!("not_in_query", outside_query.json["status"]);
    assert_eq!(Value::Null, outside_query.json["position"]);
    assert_eq!(Value::Null, outside_query.json["page"]);

    let filename_search = server
        .request("GET", "/api/collections?q=RJ123456", &[])
        .await;
    assert_eq!(1, filename_search.json["pagination"]["total"]);
    assert!(
        filename_search.json["items"][0]["filename"]
            .as_str()
            .expect("filename")
            .starts_with("RJ123456")
    );

    let quote_only = server.request("GET", "/api/collections?q=%22", &[]).await;
    assert_eq!(200, quote_only.status);
    assert_eq!(3, quote_only.json["pagination"]["total"]);

    let detail = server
        .request("GET", &format!("/api/collections/{collection_id}"), &[])
        .await;
    assert_eq!(200, detail.status);
    assert_eq!(collection_id, detail.json["id"]);
    assert_eq!("First Story", detail.json["title"]);
    assert!(
        detail.json["path"]
            .as_str()
            .expect("path")
            .ends_with(".zip")
    );
    assert_eq!(Value::Array(Vec::new()), detail.json["tags"]);

    let invalid_query = server
        .request("GET", "/api/collections?page=not-a-number", &[])
        .await;
    assert_eq!(400, invalid_query.status);
    assert_eq!("invalid_query", invalid_query.json["error"]["code"]);

    let invalid_id = server.request("GET", "/api/collections/nope", &[]).await;
    assert_eq!(400, invalid_id.status);
    assert_eq!("invalid_collection_id", invalid_id.json["error"]["code"]);

    let missing = server.request("GET", "/api/collections/999", &[]).await;
    assert_eq!(404, missing.status);
    assert_eq!("collection_not_found", missing.json["error"]["code"]);

    let missing_location = server
        .request("GET", "/api/collections/999/locate", &[])
        .await;
    assert_eq!(404, missing_location.status);
    assert_eq!(
        "collection_not_found",
        missing_location.json["error"]["code"]
    );
    server.stop().await;
}

#[tokio::test]
async fn collection_filters_require_all_metadata_and_tags_over_loopback() {
    let tree = TestTree::new("collection-filters");
    tree.zip_in("archive", "(C106) [AlphaCircle (Alice)] First Story.zip");
    tree.zip_in("archive", "(C106) [AlphaCircle (Bob)] Second Story.zip");
    tree.zip_in("downloads", "(C107) [AlphaCircle (Alice)] Third Story.zip");
    tree.zip_in("archive", "[NoAuthorCircle] Fourth Story.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.root("archive"), SourceKind::Archive, "歸檔區")
        .expect("register archive");
    repository
        .register_library_root(&tree.root("downloads"), SourceKind::Downloads, "下載區")
        .expect("register downloads");
    let roots = repository.active_scan_roots().expect("active roots");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    let report = application.run_scan(&roots).expect("scan collections");
    assert_eq!(4, report.summary.added);
    let mut repository = application.into_repository();
    let first_id = repository
        .collection_id_for_current_path(
            &tree
                .root("archive")
                .join("(C106) [AlphaCircle (Alice)] First Story.zip"),
        )
        .expect("first lookup")
        .expect("first ID");
    let second_id = repository
        .collection_id_for_current_path(
            &tree
                .root("archive")
                .join("(C106) [AlphaCircle (Bob)] Second Story.zip"),
        )
        .expect("second lookup")
        .expect("second ID");
    let third_id = repository
        .collection_id_for_current_path(
            &tree
                .root("downloads")
                .join("(C107) [AlphaCircle (Alice)] Third Story.zip"),
        )
        .expect("third lookup")
        .expect("third ID");
    for (collection_id, tags) in [
        (first_id, &["favorite", "color"][..]),
        (second_id, &["favorite"][..]),
        (third_id, &["favorite", "color"][..]),
    ] {
        for tag in tags {
            repository
                .add_collection_tag(collection_id, tag)
                .expect("add tag");
        }
    }
    let server = RunningServer::start(ApplicationService::new(repository, NoopRecycleBin)).await;

    let combined = server
        .request(
            "GET",
            "/api/collections?event=C106&circle=AlphaCircle&author=Alice&source=archive&tag=favorite&tag=color",
            &[],
        )
        .await;
    assert_eq!(200, combined.status);
    assert_eq!(1, combined.json["pagination"]["total"]);
    assert_eq!(first_id, combined.json["items"][0]["id"]);
    assert_eq!("color", combined.json["items"][0]["tags"][0]);
    assert_eq!("favorite", combined.json["items"][0]["tags"][1]);

    let all_tags = server
        .request("GET", "/api/collections?tag=favorite&tag=color", &[])
        .await;
    assert_eq!(2, all_tags.json["pagination"]["total"]);

    let missing = server
        .request("GET", "/api/collections?missing=event&missing=authors", &[])
        .await;
    assert_eq!(1, missing.json["pagination"]["total"]);
    assert_eq!("Fourth Story", missing.json["items"][0]["title"]);

    for path in [
        "/api/collections?source=other",
        "/api/collections?missing=unknown",
        "/api/collections?tag=",
    ] {
        let invalid = server.request("GET", path, &[]).await;
        assert_eq!(400, invalid.status);
        assert_eq!("invalid_collection_filter", invalid.json["error"]["code"]);
    }
    let duplicate = server
        .request(
            "GET",
            "/api/collections?circle=AlphaCircle&circle=Other",
            &[],
        )
        .await;
    assert_eq!(400, duplicate.status);
    assert_eq!("invalid_query", duplicate.json["error"]["code"]);
    server.stop().await;
}

#[tokio::test]
async fn batch_tag_and_metadata_report_each_item_and_refresh_filtered_reads() {
    let tree = TestTree::new("batch-mutations");
    tree.zip("[AlphaCircle] First Story.zip");
    tree.zip("[BetaCircle] Second Story.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "歸檔區")
        .expect("register root");
    let roots = repository.active_scan_roots().expect("active roots");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&roots).expect("scan collections");
    let first_id = application
        .repository()
        .collection_id_for_current_path(&tree.library().join("[AlphaCircle] First Story.zip"))
        .expect("first lookup")
        .expect("first ID");
    let second_id = application
        .repository()
        .collection_id_for_current_path(&tree.library().join("[BetaCircle] Second Story.zip"))
        .expect("second lookup")
        .expect("second ID");
    let server = RunningServer::start(application).await;

    let before = server
        .request("GET", "/api/collections?untagged=1", &[])
        .await;
    assert_eq!(2, before.json["pagination"]["total"]);

    let tagged = server
        .request_json(
            "POST",
            "/api/batch/tags",
            &serde_json::json!({
                "collection_ids": [first_id, second_id, 999_999],
                "name": "favorite"
            }),
        )
        .await;
    assert_eq!(200, tagged.status);
    assert_eq!(3, tagged.json["summary"]["total"]);
    assert_eq!(3, tagged.json["summary"]["completed"]);
    assert_eq!(2, tagged.json["summary"]["succeeded"]);
    assert_eq!(1, tagged.json["summary"]["failed"]);
    assert_eq!("succeeded", tagged.json["items"][0]["status"]);
    assert_eq!(
        "collection_not_found",
        tagged.json["items"][2]["error"]["code"]
    );

    let repeated = server
        .request_json(
            "POST",
            "/api/batch/tags",
            &serde_json::json!({
                "collection_ids": [first_id, second_id],
                "name": "favorite"
            }),
        )
        .await;
    assert_eq!(2, repeated.json["summary"]["unchanged"]);

    let after = server
        .request("GET", "/api/collections?untagged=1", &[])
        .await;
    assert_eq!(0, after.json["pagination"]["total"]);

    let metadata = server
        .request_json(
            "PUT",
            "/api/batch/metadata/parody",
            &serde_json::json!({
                "collection_ids": [first_id, 999_999],
                "value": "東方Project"
            }),
        )
        .await;
    assert_eq!(200, metadata.status);
    assert_eq!(1, metadata.json["summary"]["succeeded"]);
    assert_eq!(1, metadata.json["summary"]["failed"]);
    assert_eq!(
        "東方Project",
        metadata.json["items"][0]["collection"]["parody"]
    );

    let invalid = server
        .request_json(
            "PUT",
            "/api/batch/metadata/title",
            &serde_json::json!({"collection_ids": [first_id], "value": "new title"}),
        )
        .await;
    assert_eq!(400, invalid.status);
    assert_eq!(
        "unsupported_batch_metadata_field",
        invalid.json["error"]["code"]
    );
    server.stop().await;
}

#[tokio::test]
async fn manual_metadata_and_tags_follow_priority_and_validation_over_loopback() {
    let tree = TestTree::new("metadata-tags");
    tree.zip("(C77) [Circle (Alice)] filename title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "歸檔區")
        .expect("register root");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;
    let scan = server.request("POST", "/api/scans", &[]).await;
    assert_eq!(1, scan.json["summary"]["added"]);
    let listed = server.request("GET", "/api/collections", &[]).await;
    let collection_id = listed.json["items"][0]["id"]
        .as_i64()
        .expect("collection ID");
    let metadata_path = |field: &str| format!("/api/collections/{collection_id}/metadata/{field}");
    let tags_path = format!("/api/collections/{collection_id}/tags");

    let manual_title = server
        .request_json(
            "PUT",
            &metadata_path("title"),
            &serde_json::json!({"value": "  manual title  "}),
        )
        .await;
    assert_eq!(200, manual_title.status);
    assert_eq!("manual title", manual_title.json["title"]);
    let manual_search = server
        .request("GET", "/api/collections?q=manual", &[])
        .await;
    assert_eq!(1, manual_search.json["pagination"]["total"]);
    let old_search = server
        .request("GET", "/api/collections?q=filename", &[])
        .await;
    assert_eq!(1, old_search.json["pagination"]["total"]);

    let restored_title = server.request("DELETE", &metadata_path("title"), &[]).await;
    assert_eq!(200, restored_title.status);
    assert_eq!("filename title", restored_title.json["title"]);
    let repeated_clear = server.request("DELETE", &metadata_path("title"), &[]).await;
    assert_eq!("filename title", repeated_clear.json["title"]);
    let restored_search = server
        .request("GET", "/api/collections?q=filename", &[])
        .await;
    assert_eq!(1, restored_search.json["pagination"]["total"]);
    let cleared_manual_search = server
        .request("GET", "/api/collections?q=manual", &[])
        .await;
    assert_eq!(0, cleared_manual_search.json["pagination"]["total"]);

    let manual_event = server
        .request_json(
            "PUT",
            &metadata_path("event"),
            &serde_json::json!({"value": "C106"}),
        )
        .await;
    assert_eq!("C106", manual_event.json["event"]);
    let restored_event = server.request("DELETE", &metadata_path("event"), &[]).await;
    assert_eq!("C77", restored_event.json["event"]);

    let authors = server
        .request_json(
            "PUT",
            &metadata_path("authors"),
            &serde_json::json!({"value": ["Alice Updated", "Bob"]}),
        )
        .await;
    assert_eq!("Alice Updated", authors.json["authors"][0]);
    assert_eq!("Bob", authors.json["authors"][1]);

    let parody = server
        .request_json(
            "PUT",
            &metadata_path("parody"),
            &serde_json::json!({
                "value": {"raw": "Fate raw", "canonical": "Fate"}
            }),
        )
        .await;
    assert_eq!("Fate", parody.json["parody"]);
    assert_eq!("Fate raw", parody.json["parody_raw"]);

    let classification = server
        .request_json(
            "PUT",
            &metadata_path("classification"),
            &serde_json::json!({
                "value": {"top_level": "商業誌", "subcategory": "漫畫"}
            }),
        )
        .await;
    assert_eq!("商業誌", classification.json["classification_top"]);
    assert_eq!("漫畫", classification.json["classification_subcategory"]);

    let is_dl = server
        .request_json(
            "PUT",
            &metadata_path("is_dl"),
            &serde_json::json!({"value": true}),
        )
        .await;
    assert_eq!(true, is_dl.json["is_dl"]);

    let tagged = server
        .request_json(
            "POST",
            &tags_path,
            &serde_json::json!({"name": "  favorite  "}),
        )
        .await;
    assert_eq!(
        Value::Array(vec![Value::String("favorite".to_owned())]),
        tagged.json["tags"]
    );
    let repeated = server
        .request_json("POST", &tags_path, &serde_json::json!({"name": "favorite"}))
        .await;
    assert_eq!(1, repeated.json["tags"].as_array().expect("tags").len());
    let untagged = server
        .request_json(
            "DELETE",
            &tags_path,
            &serde_json::json!({"name": "favorite"}),
        )
        .await;
    assert_eq!(Value::Array(Vec::new()), untagged.json["tags"]);
    let repeated_remove = server
        .request_json(
            "DELETE",
            &tags_path,
            &serde_json::json!({"name": "favorite"}),
        )
        .await;
    assert_eq!(Value::Array(Vec::new()), repeated_remove.json["tags"]);

    let invalid_field = server
        .request_json(
            "PUT",
            &metadata_path("path"),
            &serde_json::json!({"value": "forbidden"}),
        )
        .await;
    assert_eq!(400, invalid_field.status);
    assert_eq!(
        "invalid_metadata_field",
        invalid_field.json["error"]["code"]
    );
    for (field, value) in [
        ("authors", serde_json::json!("Alice")),
        ("title", serde_json::json!("   ")),
    ] {
        let invalid = server
            .request_json(
                "PUT",
                &metadata_path(field),
                &serde_json::json!({"value": value}),
            )
            .await;
        assert_eq!(400, invalid.status);
        assert_eq!("invalid_metadata_value", invalid.json["error"]["code"]);
    }
    let empty_tag = server
        .request_json("POST", &tags_path, &serde_json::json!({"name": "   "}))
        .await;
    assert_eq!(400, empty_tag.status);
    assert_eq!("invalid_metadata_value", empty_tag.json["error"]["code"]);

    let missing = server
        .request_json(
            "PUT",
            "/api/collections/999/metadata/title",
            &serde_json::json!({"value": "missing"}),
        )
        .await;
    assert_eq!(404, missing.status);
    assert_eq!("collection_not_found", missing.json["error"]["code"]);
    server.stop().await;
}

#[tokio::test]
async fn metadata_history_exposes_candidates_selection_and_confidence_over_loopback() {
    let tree = TestTree::new("metadata-history");
    tree.zip("[Circle] filename title.zip");
    tree.zip("[Other] second title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "歸檔區")
        .expect("register root");
    let roots = repository.active_scan_roots().expect("active roots");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&roots).expect("scan collection");
    let mut repository = application.into_repository();
    let collection_id = repository
        .collection_id_for_current_path(&tree.library().join("[Circle] filename title.zip"))
        .expect("collection lookup")
        .expect("collection ID");
    let other_collection_id = repository
        .collection_id_for_current_path(&tree.library().join("[Other] second title.zip"))
        .expect("other collection lookup")
        .expect("other collection ID");
    repository
        .set_inferred_value(
            collection_id,
            MetadataField::Title,
            MetadataValue::Text("inferred title".to_owned()),
            "HTTP test inference",
        )
        .expect("set inference");
    let suggestion = repository
        .save_external_candidate(ExternalCandidate {
            collection_id,
            field: MetadataField::Title,
            value: MetadataValue::Text("external suggestion".to_owned()),
            source_reference: "provider:medium".to_owned(),
            confidence: confidence(0.8),
        })
        .expect("save suggestion");
    let ExternalCandidateOutcome::Suggestion {
        assertion_id: external_assertion_id,
        ..
    } = suggestion
    else {
        panic!("medium-confidence result must remain a suggestion");
    };
    repository
        .save_external_candidate(ExternalCandidate {
            collection_id,
            field: MetadataField::Title,
            value: MetadataValue::Text("search-only title".to_owned()),
            source_reference: "provider:low".to_owned(),
            confidence: confidence(0.5),
        })
        .expect("save search-only result");
    let manual_assertion_id = repository
        .set_manual_value(
            collection_id,
            MetadataField::Title,
            MetadataValue::Text("manual title".to_owned()),
        )
        .expect("set manual title");
    let server = RunningServer::start(ApplicationService::new(repository, NoopRecycleBin)).await;

    let history = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/metadata"),
            &[],
        )
        .await;
    assert_eq!(200, history.status);
    assert_eq!(collection_id, history.json["collection_id"]);
    let fields = history.json["fields"].as_array().expect("metadata fields");
    assert_eq!(7, fields.len());
    let title = fields
        .iter()
        .find(|field| field["field"] == "title")
        .expect("title history");
    assert_eq!(manual_assertion_id, title["selection"]["assertion_id"]);
    assert_eq!("manual", title["selection"]["selected_by"]);
    let assertions = title["assertions"].as_array().expect("assertions");
    assert_eq!(4, assertions.len());
    assert_eq!(
        1,
        assertions
            .iter()
            .filter(|assertion| assertion["selected"] == true)
            .count()
    );
    for source in ["manual", "external", "filename", "inference"] {
        assert!(
            assertions
                .iter()
                .any(|assertion| assertion["source"] == source)
        );
    }
    let external = assertions
        .iter()
        .find(|assertion| assertion["source"] == "external")
        .expect("external assertion");
    assert_eq!(external_assertion_id, external["id"]);
    assert_eq!("candidate", external["status"]);
    assert_eq!(0.8, external["confidence_total"]);
    assert_eq!(0.9, external["confidence"]["source_reliability"]);
    assert_eq!(
        "HTTP metadata history test",
        external["confidence"]["reason"]
    );

    let search_results = title["external_search_results"]
        .as_array()
        .expect("external search results");
    assert_eq!(2, search_results.len());
    assert!(
        search_results
            .iter()
            .any(|result| result["disposition"] == "suggestion")
    );
    let search_only = search_results
        .iter()
        .find(|result| result["disposition"] == "search_only")
        .expect("search-only result");
    assert_eq!(Value::Null, search_only["assertion_id"]);
    assert_eq!("provider:low", search_only["source_reference"]);
    assert_eq!("search-only title", search_only["value"]);

    let decision_path = |owner: i64, field: &str, assertion_id: i64| {
        format!("/api/collections/{owner}/metadata/{field}/assertions/{assertion_id}")
    };
    let selected = server
        .request_json(
            "PATCH",
            &decision_path(collection_id, "title", external_assertion_id),
            &serde_json::json!({"decision": "select"}),
        )
        .await;
    assert_eq!(200, selected.status);
    let selected_title = selected.json["fields"]
        .as_array()
        .expect("selected metadata fields")
        .iter()
        .find(|field| field["field"] == "title")
        .expect("selected title history");
    assert_eq!(
        external_assertion_id,
        selected_title["selection"]["assertion_id"]
    );
    assert_eq!("manual", selected_title["selection"]["selected_by"]);
    let selected_external = selected_title["assertions"]
        .as_array()
        .expect("selected assertions")
        .iter()
        .find(|assertion| assertion["id"] == external_assertion_id)
        .expect("selected external assertion");
    assert_eq!("accepted", selected_external["status"]);
    assert_eq!(true, selected_external["selected"]);
    let collection = server
        .request("GET", &format!("/api/collections/{collection_id}"), &[])
        .await;
    assert_eq!("external suggestion", collection.json["title"]);

    for path in [
        decision_path(other_collection_id, "title", external_assertion_id),
        decision_path(collection_id, "event", external_assertion_id),
    ] {
        let wrong_owner = server
            .request_json("PATCH", &path, &serde_json::json!({"decision": "reject"}))
            .await;
        assert_eq!(409, wrong_owner.status);
        assert_eq!(
            "metadata_assertion_unavailable",
            wrong_owner.json["error"]["code"]
        );
    }
    let invalid_decision = server
        .request_json(
            "PATCH",
            &decision_path(collection_id, "title", external_assertion_id),
            &serde_json::json!({"decision": "accept"}),
        )
        .await;
    assert_eq!(400, invalid_decision.status);
    assert_eq!(
        "invalid_metadata_assertion_decision",
        invalid_decision.json["error"]["code"]
    );
    let invalid_assertion_id = server
        .request_json(
            "PATCH",
            &format!("/api/collections/{collection_id}/metadata/title/assertions/nope"),
            &serde_json::json!({"decision": "select"}),
        )
        .await;
    assert_eq!(400, invalid_assertion_id.status);
    assert_eq!(
        "invalid_metadata_assertion_id",
        invalid_assertion_id.json["error"]["code"]
    );

    let rejected = server
        .request_json(
            "PATCH",
            &decision_path(collection_id, "title", external_assertion_id),
            &serde_json::json!({"decision": "reject"}),
        )
        .await;
    assert_eq!(200, rejected.status);
    let rejected_title = rejected.json["fields"]
        .as_array()
        .expect("rejected metadata fields")
        .iter()
        .find(|field| field["field"] == "title")
        .expect("rejected title history");
    assert_eq!(
        manual_assertion_id,
        rejected_title["selection"]["assertion_id"]
    );
    let rejected_external = rejected_title["assertions"]
        .as_array()
        .expect("rejected assertions")
        .iter()
        .find(|assertion| assertion["id"] == external_assertion_id)
        .expect("rejected external assertion");
    assert_eq!("rejected", rejected_external["status"]);
    assert_eq!(false, rejected_external["selected"]);
    assert_eq!(0.8, rejected_external["confidence_total"]);
    assert_eq!("HTTP metadata history test", rejected_external["reason"]);
    let collection = server
        .request("GET", &format!("/api/collections/{collection_id}"), &[])
        .await;
    assert_eq!("manual title", collection.json["title"]);
    let repeated_rejection = server
        .request_json(
            "PATCH",
            &decision_path(collection_id, "title", external_assertion_id),
            &serde_json::json!({"decision": "reject"}),
        )
        .await;
    assert_eq!(200, repeated_rejection.status);
    let reselect_rejected = server
        .request_json(
            "PATCH",
            &decision_path(collection_id, "title", external_assertion_id),
            &serde_json::json!({"decision": "select"}),
        )
        .await;
    assert_eq!(409, reselect_rejected.status);
    assert_eq!(
        "metadata_assertion_unavailable",
        reselect_rejected.json["error"]["code"]
    );

    let missing_collection = server
        .request_json(
            "PATCH",
            &decision_path(999, "title", external_assertion_id),
            &serde_json::json!({"decision": "select"}),
        )
        .await;
    assert_eq!(404, missing_collection.status);
    assert_eq!(
        "collection_not_found",
        missing_collection.json["error"]["code"]
    );

    let invalid = server
        .request("GET", "/api/collections/nope/metadata", &[])
        .await;
    assert_eq!(400, invalid.status);
    assert_eq!("invalid_collection_id", invalid.json["error"]["code"]);
    let missing = server
        .request("GET", "/api/collections/999/metadata", &[])
        .await;
    assert_eq!(404, missing.status);
    assert_eq!("collection_not_found", missing.json["error"]["code"]);
    server.stop().await;
}

#[tokio::test]
async fn external_search_jobs_can_be_enqueued_deduplicated_and_read_over_loopback() {
    let tree = TestTree::new("external-search-jobs");
    tree.zip("[Circle] filename title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "歸檔區")
        .expect("register root");
    let roots = repository.active_scan_roots().expect("active roots");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&roots).expect("scan collection");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&tree.library().join("[Circle] filename title.zip"))
        .expect("collection lookup")
        .expect("collection ID");
    let server = RunningServer::start(application).await;
    let collection_jobs_path = format!("/api/collections/{collection_id}/external-search-jobs");

    let enqueued = server
        .request_json(
            "POST",
            &collection_jobs_path,
            &serde_json::json!({"fields": ["circle", "title", "circle"]}),
        )
        .await;
    assert_eq!(200, enqueued.status);
    assert_eq!(true, enqueued.json["created"]);
    assert_eq!(collection_id, enqueued.json["job"]["collection_id"]);
    assert_eq!("pending", enqueued.json["job"]["status"]);
    assert_eq!(
        serde_json::json!(["title", "circle"]),
        enqueued.json["job"]["fields"]
    );
    assert_eq!(0, enqueued.json["job"]["attempts"]);
    assert_eq!(Value::Null, enqueued.json["job"]["result"]);
    assert_eq!(Value::Null, enqueued.json["job"]["next_retry_at"]);
    let job_id = enqueued.json["job"]["id"]
        .as_i64()
        .expect("external search job ID");

    let duplicate = server
        .request_json(
            "POST",
            &collection_jobs_path,
            &serde_json::json!({"fields": ["event"]}),
        )
        .await;
    assert_eq!(200, duplicate.status);
    assert_eq!(false, duplicate.json["created"]);
    assert_eq!(job_id, duplicate.json["job"]["id"]);
    assert_eq!(
        serde_json::json!(["title", "circle"]),
        duplicate.json["job"]["fields"]
    );

    let fetched = server
        .request("GET", &format!("/api/external-search-jobs/{job_id}"), &[])
        .await;
    assert_eq!(200, fetched.status);
    assert_eq!(job_id, fetched.json["id"]);
    assert_eq!("pending", fetched.json["status"]);

    for body in [
        serde_json::json!({"fields": []}),
        serde_json::json!({"fields": ["path"]}),
    ] {
        let invalid = server
            .request_json("POST", &collection_jobs_path, &body)
            .await;
        assert_eq!(400, invalid.status);
        assert_eq!(
            "invalid_external_search_fields",
            invalid.json["error"]["code"]
        );
    }
    let missing_collection = server
        .request_json(
            "POST",
            "/api/collections/999/external-search-jobs",
            &serde_json::json!({"fields": ["title"]}),
        )
        .await;
    assert_eq!(404, missing_collection.status);
    assert_eq!(
        "collection_not_found",
        missing_collection.json["error"]["code"]
    );
    let invalid_job_id = server
        .request("GET", "/api/external-search-jobs/nope", &[])
        .await;
    assert_eq!(400, invalid_job_id.status);
    assert_eq!(
        "invalid_external_search_job_id",
        invalid_job_id.json["error"]["code"]
    );
    let missing_job = server
        .request("GET", "/api/external-search-jobs/999", &[])
        .await;
    assert_eq!(404, missing_job.status);
    assert_eq!(
        "external_search_job_not_found",
        missing_job.json["error"]["code"]
    );
    server.stop().await;
}

#[tokio::test]
async fn external_search_status_waits_for_brief_application_contention() {
    let tree = TestTree::new("external-search-contention");
    tree.zip("[Circle] filename title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "歸檔區")
        .expect("register root");
    let roots = repository.active_scan_roots().expect("active roots");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&roots).expect("scan collection");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&tree.library().join("[Circle] filename title.zip"))
        .expect("collection lookup")
        .expect("collection ID");
    let job = application
        .enqueue_external_search(collection_id, &[MetadataField::Title])
        .expect("enqueue external search")
        .job;
    let shared = share_application(application);
    let server = RunningServer::start_shared(Arc::clone(&shared)).await;

    let held_application = Arc::clone(&shared);
    let (locked_sender, locked_receiver) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = held_application.lock().expect("hold application lock");
        locked_sender.send(()).expect("announce held lock");
        std::thread::sleep(Duration::from_millis(120));
    });
    locked_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("application lock acquired");

    let fetched = server
        .request("GET", &format!("/api/external-search-jobs/{}", job.id), &[])
        .await;
    holder.join().expect("lock holder");
    assert_eq!(200, fetched.status);
    assert_eq!(job.id, fetched.json["id"]);
    assert_eq!("pending", fetched.json["status"]);
    server.stop().await;
}

#[tokio::test]
async fn shelf_reads_wait_for_brief_application_contention() {
    let tree = TestTree::new("shelf-contention");
    tree.zip("[Circle] filename title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "歸檔區")
        .expect("register root");
    let roots = repository.active_scan_roots().expect("active roots");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&roots).expect("scan collection");
    let shared = share_application(application);
    let server = RunningServer::start_shared(Arc::clone(&shared)).await;

    let held_application = Arc::clone(&shared);
    let (locked_sender, locked_receiver) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = held_application.lock().expect("hold application lock");
        locked_sender.send(()).expect("announce held lock");
        std::thread::sleep(Duration::from_millis(120));
    });
    locked_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("application lock acquired");

    let (statistics, collections, candidates) = tokio::join!(
        server.request("GET", "/api/stats", &[]),
        server.request("GET", "/api/collections?page=1&per_page=8", &[]),
        server.request("GET", "/api/tombstone-candidates", &[]),
    );
    holder.join().expect("lock holder");

    assert_eq!(200, statistics.status);
    assert_eq!(1, statistics.json["total"]);
    assert_eq!(200, collections.status);
    assert_eq!(1, collections.json["pagination"]["total"]);
    assert_eq!(200, candidates.status);
    assert_eq!(0, candidates.json["items"].as_array().expect("items").len());
    server.stop().await;
}

struct HttpResult {
    status: u16,
    json: Value,
    headers: String,
    body: Vec<u8>,
}

impl HttpResult {
    fn parse(response: &[u8]) -> Self {
        let separator = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP body");
        let headers =
            String::from_utf8(response[..separator].to_vec()).expect("UTF-8 HTTP response headers");
        let body = response[separator + 4..].to_vec();
        let status = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse().ok())
            .expect("HTTP status");
        Self {
            status,
            json: serde_json::from_slice(&body).unwrap_or(Value::Null),
            headers,
            body,
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.lines().skip(1).find_map(|line| {
            let (header_name, value) = line.split_once(':')?;
            header_name
                .eq_ignore_ascii_case(name)
                .then_some(value.trim())
        })
    }
}

fn thumbnail_application(tree: &TestTree) -> (ApplicationService<NoopRecycleBin>, i64) {
    tree.zip("[circle] thumbnail title.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let config = ThumbnailConfig::new(tree.path.join("thumbnail-cache"), 300, 400, 80)
        .expect("thumbnail config");
    let mut application = ApplicationService::with_thumbnails(repository, NoopRecycleBin, config);
    application
        .run_scan(&[ScanRoot {
            path: tree.library(),
            source: SourceKind::Archive,
            label: "歸檔區".to_owned(),
        }])
        .expect("scan thumbnail collection");
    let collection_id = application
        .repository()
        .first_collection_id()
        .expect("collection query")
        .expect("collection ID");
    (application, collection_id)
}

#[tokio::test]
async fn thumbnail_endpoint_returns_uncacheable_placeholder_and_deduplicates_requests() {
    let tree = TestTree::new("thumbnail-placeholder");
    let (application, collection_id) = thumbnail_application(&tree);
    let server = RunningServer::start(application).await;

    for _ in 0..2 {
        let response = server
            .request(
                "GET",
                &format!("/api/collections/{collection_id}/thumbnail"),
                &[],
            )
            .await;
        assert_eq!(202, response.status);
        assert_eq!(Some("image/webp"), response.header("content-type"));
        assert_eq!(Some("no-store"), response.header("cache-control"));
        assert_eq!(Some("pending"), response.header("x-thumbnail-status"));
        assert_eq!(Some("1"), response.header("x-thumbnail-priority"));
        assert_eq!(transparent_placeholder_webp(), response.body);
    }
    let prioritized = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/thumbnail?priority=4321"),
            &[],
        )
        .await;
    assert_eq!(202, prioritized.status);
    assert_eq!(Some("4321"), prioritized.header("x-thumbnail-priority"));
    let invalid_priority = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/thumbnail?priority=0"),
            &[],
        )
        .await;
    assert_eq!(400, invalid_priority.status);
    assert_eq!(
        "invalid_thumbnail_priority",
        invalid_priority.json["error"]["code"]
    );
    let missing = server
        .request("GET", "/api/collections/999/thumbnail", &[])
        .await;
    assert_eq!(404, missing.status);
    assert_eq!("collection_not_found", missing.json["error"]["code"]);
    server.stop().await;
}

#[tokio::test]
async fn failed_thumbnail_response_exposes_frontend_retry_state() {
    let transient_tree = TestTree::new("thumbnail-transient-retry-headers");
    let (mut transient_application, transient_id) = thumbnail_application(&transient_tree);
    transient_application
        .request_thumbnail(transient_id)
        .expect("request transient thumbnail");
    transient_application
        .start_thumbnail_generation(transient_id)
        .expect("start transient thumbnail");
    transient_application
        .finish_thumbnail_generation(
            transient_id,
            Err(ThumbnailError {
                kind: ThumbnailErrorKind::SourceIo,
                message: "temporarily locked".to_owned(),
            }),
        )
        .expect("fail transient thumbnail");
    let transient_server = RunningServer::start(transient_application).await;
    let transient = transient_server
        .request(
            "GET",
            &format!("/api/collections/{transient_id}/thumbnail"),
            &[],
        )
        .await;
    assert_eq!(202, transient.status);
    assert_eq!(Some("pending"), transient.header("x-thumbnail-status"));
    assert_eq!(
        Some("source_io"),
        transient.header("x-thumbnail-error-kind")
    );
    assert!(transient.header("x-thumbnail-next-retry-at").is_some());
    transient_server.stop().await;

    let permanent_tree = TestTree::new("thumbnail-permanent-retry-headers");
    let (mut permanent_application, permanent_id) = thumbnail_application(&permanent_tree);
    permanent_application
        .request_thumbnail(permanent_id)
        .expect("request permanent thumbnail");
    permanent_application
        .start_thumbnail_generation(permanent_id)
        .expect("start permanent thumbnail");
    permanent_application
        .finish_thumbnail_generation(
            permanent_id,
            Err(ThumbnailError {
                kind: ThumbnailErrorKind::InvalidArchive,
                message: "broken archive".to_owned(),
            }),
        )
        .expect("fail permanent thumbnail");
    let permanent_server = RunningServer::start(permanent_application).await;
    let permanent = permanent_server
        .request(
            "GET",
            &format!("/api/collections/{permanent_id}/thumbnail"),
            &[],
        )
        .await;
    assert_eq!(202, permanent.status);
    assert_eq!(Some("failed"), permanent.header("x-thumbnail-status"));
    assert_eq!(
        Some("invalid_archive"),
        permanent.header("x-thumbnail-error-kind")
    );
    assert_eq!(None, permanent.header("x-thumbnail-next-retry-at"));
    permanent_server.stop().await;
}

#[tokio::test]
async fn ready_thumbnail_is_cacheable_and_manual_rebuild_invalidates_only_the_cache() {
    let tree = TestTree::new("thumbnail-ready");
    let (mut application, collection_id) = thumbnail_application(&tree);
    let state = application
        .request_thumbnail(collection_id)
        .expect("request thumbnail")
        .state;
    application
        .start_thumbnail_generation(collection_id)
        .expect("start thumbnail");
    fs::create_dir_all(state.cache_path.parent().expect("cache parent"))
        .expect("create cache directory");
    fs::write(&state.cache_path, transparent_placeholder_webp()).expect("write WebP cache");
    application
        .finish_thumbnail_generation(
            collection_id,
            Ok(ThumbnailGenerationSuccess {
                width: 1,
                height: 1,
            }),
        )
        .expect("complete thumbnail");
    let source_path = application
        .repository()
        .active_collection_file_path(collection_id)
        .expect("source path");
    let server = RunningServer::start(application).await;

    let ready = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/thumbnail"),
            &[],
        )
        .await;
    assert_eq!(200, ready.status);
    assert_eq!(
        Some("private, max-age=86400"),
        ready.header("cache-control")
    );
    assert_eq!(Some("ready"), ready.header("x-thumbnail-status"));

    let rebuilt = server
        .request(
            "POST",
            &format!("/api/collections/{collection_id}/thumbnail/rebuild"),
            &[],
        )
        .await;
    assert_eq!(200, rebuilt.status);
    assert_eq!("pending", rebuilt.json["status"]);
    assert!(
        source_path.is_file(),
        "manual rebuild must not modify source ZIP"
    );
    assert!(
        !state.cache_path.exists(),
        "manual rebuild removes only WebP cache"
    );

    let rebuilt_all = server.request("POST", "/api/thumbnails/rebuild", &[]).await;
    assert_eq!(200, rebuilt_all.status);
    assert_eq!(1, rebuilt_all.json["rebuilt"]);
    server.stop().await;
}

#[tokio::test]
async fn thumbnail_cache_job_uses_selected_roots_and_reports_progress() {
    let tree = TestTree::new("thumbnail-cache-job");
    tree.zip_in("selected", "[circle] ready.zip");
    tree.zip_in("selected", "[circle] pending.zip");
    tree.zip_in("selected", "[circle] remaining.zip");
    tree.zip_in("excluded", "[circle] excluded.zip");
    let selected_root = tree.root("selected");
    let excluded_root = tree.root("excluded");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let config = ThumbnailConfig::new(tree.path.join("thumbnail-cache"), 300, 400, 80)
        .expect("thumbnail config");
    let mut application = ApplicationService::with_thumbnails(repository, NoopRecycleBin, config);
    application
        .run_scan(&[
            ScanRoot {
                path: selected_root.clone(),
                source: SourceKind::Archive,
                label: "選取區".to_owned(),
            },
            ScanRoot {
                path: excluded_root.clone(),
                source: SourceKind::Archive,
                label: "排除區".to_owned(),
            },
        ])
        .expect("scan cache job roots");
    let selected_root_id = application
        .library_roots()
        .expect("library roots")
        .into_iter()
        .find(|root| root.label == "選取區")
        .expect("selected root")
        .id;
    let excluded_root_id = application
        .library_roots()
        .expect("library roots")
        .into_iter()
        .find(|root| root.label == "排除區")
        .expect("excluded root")
        .id;
    let ready_id = application
        .repository()
        .collection_id_for_current_path(&selected_root.join("[circle] ready.zip"))
        .expect("ready lookup")
        .expect("ready collection");
    let pending_id = application
        .repository()
        .collection_id_for_current_path(&selected_root.join("[circle] pending.zip"))
        .expect("pending lookup")
        .expect("pending collection");
    let remaining_id = application
        .repository()
        .collection_id_for_current_path(&selected_root.join("[circle] remaining.zip"))
        .expect("remaining lookup")
        .expect("remaining collection");
    let excluded_id = application
        .repository()
        .collection_id_for_current_path(&excluded_root.join("[circle] excluded.zip"))
        .expect("excluded lookup")
        .expect("excluded collection");

    let ready_state = application
        .request_thumbnail(ready_id)
        .expect("request ready thumbnail")
        .state;
    application
        .start_thumbnail_generation(ready_id)
        .expect("start ready thumbnail");
    fs::create_dir_all(ready_state.cache_path.parent().expect("cache parent"))
        .expect("create cache directory");
    fs::write(&ready_state.cache_path, transparent_placeholder_webp()).expect("write ready cache");
    application
        .finish_thumbnail_generation(
            ready_id,
            Ok(ThumbnailGenerationSuccess {
                width: 1,
                height: 1,
            }),
        )
        .expect("finish ready thumbnail");

    let shared = share_application(application);
    let server = RunningServer::start_shared(Arc::clone(&shared)).await;
    let preflight = server
        .request_json(
            "POST",
            "/api/thumbnail-cache-jobs/preflight",
            &serde_json::json!({ "root_ids": [selected_root_id] }),
        )
        .await;
    assert_eq!(200, preflight.status);
    assert_eq!(1, preflight.json["root_count"]);
    assert_eq!(3, preflight.json["collection_count"]);
    assert_eq!(1, preflight.json["ready"]);
    assert_eq!(2, preflight.json["requires_build"]);
    assert_eq!(false, preflight.json["cancellation_supported"]);
    assert!(
        shared
            .lock()
            .expect("application")
            .repository()
            .thumbnail_state(pending_id)
            .is_err(),
        "preflight must not enqueue thumbnail work"
    );
    fs::remove_file(&ready_state.cache_path).expect("remove ready cache for preflight");
    let missing_cache_preflight = server
        .request_json(
            "POST",
            "/api/thumbnail-cache-jobs/preflight",
            &serde_json::json!({ "root_ids": [selected_root_id] }),
        )
        .await;
    assert_eq!(0, missing_cache_preflight.json["ready"]);
    assert_eq!(3, missing_cache_preflight.json["requires_build"]);
    fs::write(&ready_state.cache_path, transparent_placeholder_webp())
        .expect("restore ready cache after preflight");
    let started = server
        .request_json(
            "POST",
            "/api/thumbnail-cache-jobs",
            &serde_json::json!({ "root_ids": [selected_root_id] }),
        )
        .await;
    assert_eq!(200, started.status);
    assert_eq!(
        serde_json::json!([selected_root_id]),
        started.json["root_ids"]
    );
    assert_eq!("running", started.json["status"]);
    assert_eq!(3, started.json["total"]);
    assert_eq!(1, started.json["ready"]);
    assert_eq!(2, started.json["pending"]);
    assert_eq!(Some(33.3), started.json["progress_percent"].as_f64());
    assert!(started.json["estimated_seconds_remaining"].is_null());
    assert_eq!(
        transparent_placeholder_webp(),
        fs::read(&ready_state.cache_path).expect("ready cache retained")
    );
    assert!(
        shared
            .lock()
            .expect("application")
            .repository()
            .thumbnail_state(excluded_id)
            .is_err(),
        "excluded root must not be queued"
    );

    let overlapping = server
        .request_json(
            "POST",
            "/api/thumbnail-cache-jobs",
            &serde_json::json!({ "root_ids": [excluded_root_id] }),
        )
        .await;
    assert_eq!(409, overlapping.status);
    assert_eq!(
        "thumbnail_cache_job_running",
        overlapping.json["error"]["code"]
    );

    {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut application = shared.lock().expect("application");
        let request = application
            .start_thumbnail_generation(pending_id)
            .expect("start pending thumbnail");
        fs::write(&request.cache_path, transparent_placeholder_webp())
            .expect("write pending cache");
        application
            .finish_thumbnail_generation(
                pending_id,
                Ok(ThumbnailGenerationSuccess {
                    width: 1,
                    height: 1,
                }),
            )
            .expect("finish pending thumbnail");
    }

    let estimating = server
        .request("GET", "/api/thumbnail-cache-jobs/current", &[])
        .await;
    assert_eq!("running", estimating.json["job"]["status"]);
    assert_eq!(2, estimating.json["job"]["ready"]);
    assert_eq!(1, estimating.json["job"]["pending"]);
    assert!(
        estimating.json["job"]["estimated_seconds_remaining"]
            .as_u64()
            .is_some_and(|seconds| seconds > 0),
        "ETA becomes available after the first newly completed thumbnail"
    );

    {
        let mut application = shared.lock().expect("application");
        let request = application
            .start_thumbnail_generation(remaining_id)
            .expect("start remaining thumbnail");
        fs::write(&request.cache_path, transparent_placeholder_webp())
            .expect("write remaining cache");
        application
            .finish_thumbnail_generation(
                remaining_id,
                Ok(ThumbnailGenerationSuccess {
                    width: 1,
                    height: 1,
                }),
            )
            .expect("finish remaining thumbnail");
    }

    let completed = server
        .request("GET", "/api/thumbnail-cache-jobs/current", &[])
        .await;
    assert_eq!(200, completed.status);
    assert_eq!("completed", completed.json["job"]["status"]);
    assert_eq!(3, completed.json["job"]["ready"]);
    assert_eq!(
        Some(100.0),
        completed.json["job"]["progress_percent"].as_f64()
    );
    assert_eq!(0, completed.json["job"]["estimated_seconds_remaining"]);

    let next = server
        .request_json(
            "POST",
            "/api/thumbnail-cache-jobs",
            &serde_json::json!({ "root_ids": [excluded_root_id] }),
        )
        .await;
    assert_eq!(200, next.status);
    assert_eq!(1, next.json["total"]);
    assert!(next.json["id"].as_u64() > started.json["id"].as_u64());

    let invalid = server
        .request_json(
            "POST",
            "/api/thumbnail-cache-jobs",
            &serde_json::json!({ "root_ids": [] }),
        )
        .await;
    assert_eq!(
        409, invalid.status,
        "running job is rejected before new scope"
    );
    server.stop().await;
}

#[tokio::test]
async fn thumbnail_cache_failures_can_be_listed_and_requeued() {
    let tree = TestTree::new("thumbnail-cache-failure-retry");
    let (mut application, collection_id) = thumbnail_application(&tree);
    let root_id = application
        .library_roots()
        .expect("library roots")
        .into_iter()
        .next()
        .expect("thumbnail root")
        .id;
    application
        .request_thumbnail(collection_id)
        .expect("request thumbnail");
    application
        .start_thumbnail_generation(collection_id)
        .expect("start thumbnail");
    application
        .finish_thumbnail_generation(
            collection_id,
            Err(ThumbnailError {
                kind: ThumbnailErrorKind::InvalidArchive,
                message: "broken archive".to_owned(),
            }),
        )
        .expect("fail thumbnail");
    let shared = share_application(application);
    let server = RunningServer::start_shared(Arc::clone(&shared)).await;

    let started = server
        .request_json(
            "POST",
            "/api/thumbnail-cache-jobs",
            &serde_json::json!({ "root_ids": [root_id] }),
        )
        .await;
    assert_eq!(200, started.status);
    assert_eq!("completed_with_errors", started.json["status"]);
    assert_eq!(1, started.json["failed"]);
    assert_eq!(
        serde_json::json!([collection_id]),
        started.json["failed_collection_ids"]
    );

    let failures = server
        .request("GET", "/api/thumbnail-cache-jobs/current/failures", &[])
        .await;
    assert_eq!(200, failures.status);
    assert_eq!(started.json["id"], failures.json["job_id"]);
    assert_eq!(collection_id, failures.json["items"][0]["id"]);
    assert_eq!(
        Value::Array(Vec::new()),
        failures.json["missing_collection_ids"]
    );

    let retried = server
        .request(
            "POST",
            "/api/thumbnail-cache-jobs/current/retry-failures",
            &[],
        )
        .await;
    assert_eq!(200, retried.status);
    assert_eq!("running", retried.json["status"]);
    assert_eq!(1, retried.json["pending"]);
    assert_eq!(0, retried.json["failed"]);
    assert!(retried.json["id"].as_u64() > started.json["id"].as_u64());
    assert_eq!(
        ThumbnailStatus::Pending,
        shared
            .lock()
            .expect("application")
            .repository()
            .thumbnail_state(collection_id)
            .expect("retried thumbnail state")
            .status
    );
    server.stop().await;
}

#[tokio::test]
async fn settings_api_validates_persists_and_requeues_existing_thumbnail_state() {
    let tree = TestTree::new("settings-api");
    let (mut application, collection_id) = thumbnail_application(&tree);
    application
        .request_thumbnail(collection_id)
        .expect("create existing thumbnail state");
    let reader_path = tree.root("reader.exe");
    let server = RunningServer::start(application).await;

    let initial = server.request("GET", "/api/settings", &[]).await;
    assert_eq!(200, initial.status);
    assert_eq!("300x400", initial.json["thumb_size"]);
    assert_eq!(80, initial.json["thumb_quality"]);
    assert_eq!(serde_json::json!([]), initial.json["environment_overrides"]);

    let updated = server
        .request_json(
            "PUT",
            "/api/settings",
            &serde_json::json!({
                "viewer_path": reader_path.to_string_lossy(),
                "thumb_size": "360x480",
                "thumb_quality": 85
            }),
        )
        .await;
    assert_eq!(200, updated.status);
    assert_eq!("360x480", updated.json["thumb_size"]);
    assert_eq!(85, updated.json["thumb_quality"]);
    assert_eq!(1, updated.json["thumbnails_requeued"]);

    for payload in [
        serde_json::json!({
            "viewer_path": "",
            "thumb_size": "300*400",
            "thumb_quality": 80
        }),
        serde_json::json!({
            "viewer_path": "",
            "thumb_size": "300x400",
            "thumb_quality": 0
        }),
        serde_json::json!({
            "viewer_path": "",
            "thumb_size": "300x400",
            "thumb_quality": 80,
            "unknown": true
        }),
    ] {
        let invalid = server.request_json("PUT", "/api/settings", &payload).await;
        assert_eq!(400, invalid.status);
    }
    let retained = server.request("GET", "/api/settings", &[]).await;
    assert_eq!("360x480", retained.json["thumb_size"]);
    assert_eq!(85, retained.json["thumb_quality"]);
    server.stop().await;
}

#[tokio::test]
async fn settings_api_identifies_each_environment_override_and_saved_fallback() {
    let tree = TestTree::new("settings-api-overrides");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let environment_reader = tree.root("environment-reader.exe");
    let saved_reader = tree.root("saved-reader.exe");
    let thumbnail_config =
        ThumbnailConfig::new(tree.path.join("cache"), 500, 600, 90).expect("thumbnail config");
    let launcher = RecordingLauncher::new(false);
    let mut application = ApplicationService::with_launcher_thumbnails_and_overrides(
        repository,
        NoopRecycleBin,
        launcher,
        Some(environment_reader.clone()),
        thumbnail_config,
        ApplicationSettingsOverrides {
            reader_path: Some(environment_reader.clone()),
            thumbnail_size: Some((500, 600)),
            thumbnail_quality: Some(90),
        },
    );
    application
        .save_application_settings(Some(saved_reader.clone()), 360, 480, 85)
        .expect("save fallback settings");
    let server = RunningServer::start(application).await;

    let response = server.request("GET", "/api/settings", &[]).await;
    assert_eq!(200, response.status);
    assert_eq!(
        environment_reader.to_string_lossy().as_ref(),
        response.json["viewer_path"]
    );
    assert_eq!("500x600", response.json["thumb_size"]);
    assert_eq!(90, response.json["thumb_quality"]);
    assert_eq!(
        saved_reader.to_string_lossy().as_ref(),
        response.json["saved_viewer_path"]
    );
    assert_eq!("360x480", response.json["saved_thumb_size"]);
    assert_eq!(85, response.json["saved_thumb_quality"]);
    assert_eq!(
        "DOUJIN_READER_PATH",
        response.json["overrides"]["viewer_path"]
    );
    assert_eq!(
        "DOUJIN_THUMB_SIZE",
        response.json["overrides"]["thumb_size"]
    );
    assert_eq!(
        "DOUJIN_THUMB_QUALITY",
        response.json["overrides"]["thumb_quality"]
    );
    server.stop().await;
}

#[tokio::test]
async fn statistics_api_reports_categories_tags_and_common_metadata() {
    let tree = TestTree::new("statistics-api");
    tree.zip("[Circle (Author)] first.zip");
    tree.zip("[Circle (Author)] second.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[ScanRoot {
            path: tree.library(),
            source: SourceKind::Archive,
            label: "歸檔區".to_owned(),
        }])
        .expect("scan collections");
    let collection_ids = application
        .repository()
        .collections(&doujin_storage::collections::CollectionQuery::default())
        .expect("collections")
        .items
        .into_iter()
        .map(|collection| collection.id)
        .collect::<Vec<_>>();
    application
        .add_collection_tag(collection_ids[0], "favorite")
        .expect("tag collection");
    for collection_id in &collection_ids {
        application
            .set_manual_metadata(
                *collection_id,
                MetadataField::Event,
                MetadataValue::Text("C100".to_owned()),
            )
            .expect("set event");
        application
            .set_manual_metadata(
                *collection_id,
                MetadataField::Parody,
                MetadataValue::Parody(doujin_parser::domain::Parody {
                    raw: "Work".to_owned(),
                    canonical: "Work".to_owned(),
                    evidence: "manual test".to_owned(),
                }),
            )
            .expect("set parody");
    }
    let server = RunningServer::start(application).await;

    let statistics = server.request("GET", "/api/stats", &[]).await;

    assert_eq!(200, statistics.status);
    assert_eq!(2, statistics.json["total"]);
    assert_eq!(1, statistics.json["tagged"]);
    assert_eq!(0, statistics.json["missing_metadata"]);
    assert_eq!("同人誌", statistics.json["categories"][0]["name"]);
    assert_eq!(2, statistics.json["categories"][0]["count"]);
    assert_eq!("Author", statistics.json["top_author"][0]["name"]);
    assert_eq!(2, statistics.json["top_author"][0]["count"]);
    assert_eq!("Circle", statistics.json["top_circle"][0]["name"]);
    assert_eq!(2, statistics.json["top_circle"][0]["count"]);
    assert_eq!("Work", statistics.json["top_parody"][0]["name"]);
    assert_eq!(2, statistics.json["top_parody"][0]["count"]);
    assert_eq!("C100", statistics.json["top_event"][0]["name"]);
    assert_eq!(2, statistics.json["top_event"][0]["count"]);
    assert_eq!("favorite", statistics.json["top_tags"][0]["name"]);
    assert_eq!(1, statistics.json["top_tags"][0]["count"]);

    let author_facets = server
        .request("GET", "/api/facets?field=author&q=Auth", &[])
        .await;
    assert_eq!(200, author_facets.status);
    assert_eq!("Author", author_facets.json["items"][0]["name"]);
    assert_eq!(2, author_facets.json["items"][0]["count"]);
    let tag_facets = server
        .request("GET", "/api/facets?field=tag&q=fav&limit=1", &[])
        .await;
    assert_eq!("favorite", tag_facets.json["items"][0]["name"]);
    assert_eq!(1, tag_facets.json["items"][0]["count"]);
    for path in ["/api/facets", "/api/facets?field=unknown"] {
        let invalid = server.request("GET", path, &[]).await;
        assert_eq!(400, invalid.status);
    }
    server.stop().await;
}

#[tokio::test]
async fn facets_merge_case_variants_and_match_collection_filter_totals() {
    let tree = TestTree::new("facet-case-variants");
    tree.zip("first.zip");
    tree.zip("second.zip");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[ScanRoot {
            path: tree.library(),
            source: SourceKind::Archive,
            label: "歸檔區".to_owned(),
        }])
        .expect("scan collections");
    let collection_ids = application
        .repository()
        .collections(&doujin_storage::collections::CollectionQuery::default())
        .expect("collections")
        .items
        .into_iter()
        .map(|collection| collection.id)
        .collect::<Vec<_>>();
    assert_eq!(2, collection_ids.len());

    for (collection_id, event) in collection_ids.iter().zip(["Comiket", "comiket"]) {
        application
            .set_manual_metadata(
                *collection_id,
                MetadataField::Event,
                MetadataValue::Text(event.to_owned()),
            )
            .expect("set event");
    }

    let server = RunningServer::start(application).await;
    let facets = server.request("GET", "/api/facets?field=event", &[]).await;
    assert_eq!(200, facets.status);
    let items = facets.json["items"].as_array().expect("facet items");
    assert_eq!(1, items.len());
    assert_eq!("Comiket", items[0]["name"]);
    assert_eq!(2, items[0]["count"]);

    let collections = server
        .request("GET", "/api/collections?event=Comiket", &[])
        .await;
    assert_eq!(200, collections.status);
    assert_eq!(2, collections.json["pagination"]["total"]);
    server.stop().await;
}

#[tokio::test]
async fn bind_configuration_accepts_only_ipv4_or_ipv6_loopback() {
    assert!(
        validate_loopback_address(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5000)).is_ok()
    );
    assert!(
        validate_loopback_address(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 5000)).is_ok()
    );
    assert!(validate_loopback_address(SocketAddr::from(([0, 0, 0, 0], 5000))).is_err());
    assert!(validate_loopback_address(SocketAddr::from(([192, 168, 1, 20], 5000))).is_err());
}

#[tokio::test]
async fn health_and_scan_endpoints_work_over_a_real_loopback_socket() {
    let tree = TestTree::new("scan");
    tree.zip("[circle] title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Downloads, "下載區")
        .expect("register root");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;

    let health = server.request("GET", "/api/health", &[]).await;
    assert_eq!(200, health.status);
    assert_eq!("ok", health.json["status"]);

    let rebinding = server
        .request("GET", "/api/health", &[("Host", "attacker.example")])
        .await;
    assert_eq!(421, rebinding.status);
    assert_eq!("invalid_host", rebinding.json["error"]["code"]);

    let scan = server.request("POST", "/api/scans", &[]).await;
    assert_eq!(200, scan.status);
    assert_eq!("succeeded", scan.json["status"]);
    assert_eq!(1, scan.json["summary"]["added"]);
    let scan_run_id = scan.json["scan_run_id"].as_i64().expect("scan run ID");

    let persisted = server
        .request("GET", &format!("/api/scans/{scan_run_id}"), &[])
        .await;
    assert_eq!(200, persisted.status);
    assert_eq!("succeeded", persisted.json["status"]);
    assert_eq!(1, persisted.json["summary"]["added"]);
    assert_eq!(Value::Array(Vec::new()), persisted.json["issues"]);
    let latest = server.request("GET", "/api/scans/latest", &[]).await;
    assert_eq!(200, latest.status);
    assert_eq!(scan_run_id, latest.json["scan"]["id"]);
    assert_eq!("succeeded", latest.json["scan"]["status"]);
    server.stop().await;
}

#[tokio::test]
async fn missing_configured_root_returns_a_persisted_partial_scan() {
    let tree = TestTree::new("missing-root");
    fs::create_dir_all(tree.library()).expect("create library");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "暫時來源")
        .expect("register root");
    fs::remove_dir(tree.library()).expect("remove configured root");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;

    let scan = server.request("POST", "/api/scans", &[]).await;

    assert_eq!(200, scan.status);
    assert_eq!("partial", scan.json["status"]);
    assert_eq!(1, scan.json["summary"]["missing_roots"]);
    assert_eq!("missing_root", scan.json["issues"][0]["kind"]);
    let scan_run_id = scan.json["scan_run_id"].as_i64().expect("scan run ID");
    let persisted = server
        .request("GET", &format!("/api/scans/{scan_run_id}"), &[])
        .await;
    assert_eq!("partial", persisted.json["status"]);
    assert_eq!("missing_root", persisted.json["issues"][0]["kind"]);
    let latest = server.request("GET", "/api/scans/latest", &[]).await;
    assert_eq!(scan_run_id, latest.json["scan"]["id"]);
    assert_eq!("missing_root", latest.json["scan"]["issues"][0]["kind"]);
    server.stop().await;
}

#[tokio::test]
async fn tombstone_candidates_can_be_listed_and_decided_without_merging_identity() {
    let tree = TestTree::new("tombstone-candidates");
    let filename = "[circle] same-name.zip";
    tree.zip(filename);
    let old_path = tree.library().join(filename);
    let root = ScanRoot {
        path: tree.library(),
        source: SourceKind::Downloads,
        label: "下載區".to_owned(),
    };
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(std::slice::from_ref(&root))
        .expect("initial scan");
    let old_id = application
        .repository()
        .collection_id_for_current_path(&old_path)
        .expect("old path lookup")
        .expect("old collection");
    fs::remove_file(&old_path).expect("remove old collection");
    tree.zip_in("library/candidate-a", filename);
    tree.zip_in("library/candidate-b", filename);
    let scan = application.run_scan(&[root]).expect("reconciliation scan");
    assert_eq!(1, scan.summary.tombstoned);
    assert_eq!(2, scan.summary.candidate_links_created);
    let server = RunningServer::start(application).await;

    let listed = server
        .request("GET", "/api/tombstone-candidates", &[])
        .await;
    assert_eq!(200, listed.status);
    let items = listed.json["items"].as_array().expect("candidate items");
    assert_eq!(2, items.len());
    assert!(items.iter().all(|item| item["decision"] == "pending"));
    assert!(
        items
            .iter()
            .all(|item| item["tombstone_collection_id"] == old_id)
    );
    let candidate_id = items[0]["candidate_collection_id"]
        .as_i64()
        .expect("candidate ID");

    let decided = server
        .request_json(
            "PATCH",
            &format!("/api/tombstone-candidates/{old_id}/{candidate_id}"),
            &serde_json::json!({ "decision": "rejected" }),
        )
        .await;
    assert_eq!(200, decided.status);
    assert_eq!("rejected", decided.json["decision"]);
    assert!(decided.json["decided_at"].is_string());

    let candidate_still_active = server
        .request("GET", &format!("/api/collections/{candidate_id}"), &[])
        .await;
    assert_eq!(200, candidate_still_active.status);
    let tombstone_hidden = server
        .request("GET", &format!("/api/collections/{old_id}"), &[])
        .await;
    assert_eq!(404, tombstone_hidden.status);

    let invalid = server
        .request_json(
            "PATCH",
            &format!("/api/tombstone-candidates/{old_id}/{candidate_id}"),
            &serde_json::json!({ "decision": "pending" }),
        )
        .await;
    assert_eq!(400, invalid.status);
    assert_eq!(
        "invalid_tombstone_candidate_decision",
        invalid.json["error"]["code"]
    );
    server.stop().await;
}

#[tokio::test]
async fn consolidation_preflight_resolves_manual_conflict_and_redirects_merged_id() {
    let tree = TestTree::new("consolidation");
    let filename = "[circle] consolidate.zip";
    tree.zip(filename);
    let old_path = tree.library().join(filename);
    let root = ScanRoot {
        path: tree.library(),
        source: SourceKind::Downloads,
        label: "下載區".to_owned(),
    };
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(std::slice::from_ref(&root))
        .expect("initial scan");
    let old_id = application
        .repository()
        .collection_id_for_current_path(&old_path)
        .expect("old lookup")
        .expect("old ID");
    application
        .set_manual_metadata(
            old_id,
            MetadataField::Title,
            MetadataValue::Text("舊手動標題".to_owned()),
        )
        .expect("old manual title");
    application
        .add_collection_tag(old_id, "old-tag")
        .expect("old tag");
    fs::remove_file(&old_path).expect("remove old");
    tree.zip_in("library/new", filename);
    let candidate_path = tree.root("library/new").join(filename);
    application.run_scan(&[root]).expect("reconciliation scan");
    let candidate_id = application
        .repository()
        .collection_id_for_current_path(&candidate_path)
        .expect("candidate lookup")
        .expect("candidate ID");
    application
        .set_manual_metadata(
            candidate_id,
            MetadataField::Title,
            MetadataValue::Text("新手動標題".to_owned()),
        )
        .expect("candidate manual title");
    application
        .add_collection_tag(candidate_id, "new-tag")
        .expect("candidate tag");
    application
        .decide_tombstone_candidate(
            old_id,
            candidate_id,
            doujin_storage::lifecycle::CandidateDecision::Confirmed,
        )
        .expect("confirm candidate");
    let server = RunningServer::start(application).await;
    let base = format!("/api/tombstone-candidates/{old_id}/{candidate_id}");

    let preflight = server
        .request("GET", &format!("{base}/preflight"), &[])
        .await;
    assert_eq!(200, preflight.status);
    assert_eq!(false, preflight.json["ready"]);
    assert_eq!(
        0,
        preflight.json["blockers"]
            .as_array()
            .expect("blockers")
            .len()
    );
    assert_eq!("title", preflight.json["conflicts"][0]["field"]);
    assert_eq!(
        "舊手動標題",
        preflight.json["conflicts"][0]["tombstone"]["value"]
    );
    assert_eq!(
        "新手動標題",
        preflight.json["conflicts"][0]["candidate"]["value"]
    );

    let blocked = server
        .request_json(
            "POST",
            &format!("{base}/consolidate"),
            &serde_json::json!({ "resolutions": [] }),
        )
        .await;
    assert_eq!(409, blocked.status);
    assert_eq!("invalid_lifecycle", blocked.json["error"]["code"]);

    let consolidated = server
        .request_json(
            "POST",
            &format!("{base}/consolidate"),
            &serde_json::json!({
                "resolutions": [{ "field": "title", "choice": "candidate" }]
            }),
        )
        .await;
    assert_eq!(200, consolidated.status);
    assert_eq!(old_id, consolidated.json["survivor_collection_id"]);
    assert_eq!(candidate_id, consolidated.json["merged_collection_id"]);
    assert_eq!(false, consolidated.json["already_completed"]);
    assert_eq!("candidate", consolidated.json["resolutions"][0]["choice"]);

    let survivor = server
        .request("GET", &format!("/api/collections/{old_id}"), &[])
        .await;
    assert_eq!(200, survivor.status);
    assert_eq!(
        candidate_path.canonicalize().expect("candidate path"),
        PathBuf::from(survivor.json["path"].as_str().expect("survivor path"))
            .canonicalize()
            .expect("stored survivor path")
    );
    assert_eq!("新手動標題", survivor.json["title"]);
    assert_eq!(
        serde_json::json!(["new-tag", "old-tag"]),
        survivor.json["tags"]
    );
    let merged = server
        .request("GET", &format!("/api/collections/{candidate_id}"), &[])
        .await;
    assert_eq!(410, merged.status);
    assert_eq!("collection_merged", merged.json["error"]["code"]);
    assert_eq!(old_id, merged.json["error"]["merged_into_collection_id"]);
    assert!(candidate_path.is_file());

    let repeated = server
        .request_json(
            "POST",
            &format!("{base}/consolidate"),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(200, repeated.status);
    assert_eq!(true, repeated.json["already_completed"]);
    assert_eq!(
        consolidated.json["consolidation_id"],
        repeated.json["consolidation_id"]
    );
    let immutable_decision = server
        .request_json(
            "PATCH",
            &base,
            &serde_json::json!({ "decision": "rejected" }),
        )
        .await;
    assert_eq!(409, immutable_decision.status);
    server.stop().await;
}

#[tokio::test]
async fn open_and_read_apis_use_only_the_validated_collection_and_configured_reader() {
    let tree = TestTree::new("open-read-api");
    let filename = "[circle] readable.zip";
    tree.zip(filename);
    let collection_path = tree.library().join(filename);
    let reader_path = tree.root("reader.exe");
    fs::write(&reader_path, b"fake reader").expect("create fake reader");
    let launcher = RecordingLauncher::new(false);
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::with_launcher(
        repository,
        NoopRecycleBin,
        launcher.clone(),
        Some(reader_path.clone()),
    );
    application
        .run_scan(&[ScanRoot {
            path: tree.library(),
            source: SourceKind::Archive,
            label: "歸檔區".to_owned(),
        }])
        .expect("scan collection");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&collection_path)
        .expect("collection lookup")
        .expect("collection");
    let server = RunningServer::start(application).await;

    let opened = server
        .request(
            "POST",
            &format!("/api/collections/{collection_id}/open"),
            &[],
        )
        .await;
    assert_eq!(200, opened.status);
    assert_eq!(collection_id, opened.json["collection_id"]);
    assert_eq!("system_default", opened.json["action"]);
    assert_eq!(true, opened.json["launched"]);

    let read = server
        .request(
            "POST",
            &format!("/api/collections/{collection_id}/read"),
            &[],
        )
        .await;
    assert_eq!(200, read.status);
    assert_eq!("configured_reader", read.json["action"]);
    assert_eq!(
        vec![
            LaunchCall {
                reader: None,
                path: collection_path.clone(),
            },
            LaunchCall {
                reader: Some(reader_path),
                path: collection_path.clone(),
            },
        ],
        launcher.calls()
    );

    fs::remove_file(&collection_path).expect("remove indexed file");
    let missing = server
        .request(
            "POST",
            &format!("/api/collections/{collection_id}/open"),
            &[],
        )
        .await;
    assert_eq!(404, missing.status);
    assert_eq!("collection_file_not_found", missing.json["error"]["code"]);
    assert_eq!(2, launcher.calls().len());
    server.stop().await;
}

#[tokio::test]
async fn read_api_rejects_unconfigured_or_missing_reader_without_launching() {
    let tree = TestTree::new("reader-validation");
    let filename = "[circle] reader validation.zip";
    tree.zip(filename);
    let collection_path = tree.library().join(filename);
    let root = ScanRoot {
        path: tree.library(),
        source: SourceKind::Archive,
        label: "歸檔區".to_owned(),
    };

    let launcher = RecordingLauncher::new(false);
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application =
        ApplicationService::with_launcher(repository, NoopRecycleBin, launcher.clone(), None);
    application
        .run_scan(std::slice::from_ref(&root))
        .expect("scan collection");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&collection_path)
        .expect("collection lookup")
        .expect("collection");
    let server = RunningServer::start(application).await;
    let unconfigured = server
        .request(
            "POST",
            &format!("/api/collections/{collection_id}/read"),
            &[],
        )
        .await;
    assert_eq!(404, unconfigured.status);
    assert_eq!("reader_not_configured", unconfigured.json["error"]["code"]);
    assert!(launcher.calls().is_empty());
    server.stop().await;

    let missing_launcher = RecordingLauncher::new(false);
    let repository = CatalogRepository::open_in_memory().expect("open second catalog");
    let mut application = ApplicationService::with_launcher(
        repository,
        NoopRecycleBin,
        missing_launcher.clone(),
        Some(tree.root("missing-reader.exe")),
    );
    application.run_scan(&[root]).expect("scan second catalog");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&collection_path)
        .expect("second collection lookup")
        .expect("second collection");
    let server = RunningServer::start(application).await;
    let missing = server
        .request(
            "POST",
            &format!("/api/collections/{collection_id}/read"),
            &[],
        )
        .await;
    assert_eq!(404, missing.status);
    assert_eq!("reader_not_found", missing.json["error"]["code"]);
    assert!(missing_launcher.calls().is_empty());
    server.stop().await;
}

#[tokio::test]
async fn launcher_failure_is_not_reported_as_a_successful_open() {
    let tree = TestTree::new("launcher-failure");
    let filename = "[circle] launcher failure.zip";
    tree.zip(filename);
    let collection_path = tree.library().join(filename);
    let launcher = RecordingLauncher::new(true);
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application =
        ApplicationService::with_launcher(repository, NoopRecycleBin, launcher.clone(), None);
    application
        .run_scan(&[ScanRoot {
            path: tree.library(),
            source: SourceKind::Archive,
            label: "歸檔區".to_owned(),
        }])
        .expect("scan collection");
    let collection_id = application
        .repository()
        .collection_id_for_current_path(&collection_path)
        .expect("collection lookup")
        .expect("collection");
    let server = RunningServer::start(application).await;

    let response = server
        .request(
            "POST",
            &format!("/api/collections/{collection_id}/open"),
            &[],
        )
        .await;
    assert_eq!(500, response.status);
    assert_eq!("external_launch_failed", response.json["error"]["code"]);
    assert!(launcher.calls().is_empty());
    server.stop().await;
}

#[tokio::test]
async fn move_api_derives_safe_destinations_and_reports_each_item() {
    let tree = TestTree::new("file-move-api");
    let first_name = "(C106) [circle] first.zip";
    let second_name = "(C106) [circle] second.zip";
    tree.zip_in("downloads", first_name);
    tree.zip_in("downloads", second_name);
    let downloads = tree.root("downloads");
    let archive = tree.root("archive");
    fs::create_dir(&archive).expect("create archive");
    let root = ScanRoot {
        path: downloads.clone(),
        source: SourceKind::Downloads,
        label: "下載區".to_owned(),
    };
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&[root]).expect("scan downloads");
    let first_path = downloads.join(first_name);
    let second_path = downloads.join(second_name);
    let first_id = application
        .repository()
        .collection_id_for_current_path(&first_path)
        .expect("first lookup")
        .expect("first collection");
    let second_id = application
        .repository()
        .collection_id_for_current_path(&second_path)
        .expect("second lookup")
        .expect("second collection");
    application
        .set_manual_metadata(
            first_id,
            MetadataField::Event,
            MetadataValue::Text("CON".to_owned()),
        )
        .expect("set reserved event");
    let archive_root_id = application
        .register_library_root(&archive, SourceKind::Archive, "歸檔區")
        .expect("register archive")
        .id;
    let downloads_root_id = application
        .library_roots()
        .expect("library roots")
        .into_iter()
        .find(|library_root| library_root.source == SourceKind::Downloads)
        .expect("downloads root")
        .id;
    let second_event = application
        .collection(second_id)
        .expect("second collection snapshot")
        .event
        .expect("parsed event");
    let second_destination_directory = archive.join(second_event);
    fs::create_dir(&second_destination_directory).expect("create conflict directory");
    let second_destination = second_destination_directory.join(second_name);
    fs::write(&second_destination, b"existing archive file").expect("create conflict");
    let server = RunningServer::start(application).await;

    let arbitrary_path = server
        .request_json(
            "POST",
            "/api/file-actions/move",
            &serde_json::json!({
                "collection_ids": [first_id],
                "archive_root_id": archive_root_id,
                "destination": tree.root("outside")
            }),
        )
        .await;
    assert_eq!(400, arbitrary_path.status);
    assert_eq!("invalid_json", arbitrary_path.json["error"]["code"]);
    assert!(first_path.is_file());

    let moved = server
        .request_json(
            "POST",
            "/api/file-actions/move",
            &serde_json::json!({
                "collection_ids": [first_id, second_id],
                "archive_root_id": archive_root_id
            }),
        )
        .await;
    assert_eq!(200, moved.status);
    assert_eq!(1, moved.json["succeeded"]);
    assert_eq!(1, moved.json["failed"]);
    assert_eq!(0, moved.json["pending_recovery"]);
    assert_eq!("succeeded", moved.json["items"][0]["status"]);
    assert_eq!("failed", moved.json["items"][1]["status"]);
    assert!(!first_path.exists());
    assert!(archive.join("_CON").join(first_name).is_file());
    assert!(second_path.is_file());
    assert_eq!(
        b"existing archive file",
        fs::read(&second_destination)
            .expect("read conflict")
            .as_slice()
    );

    let invalid_root = server
        .request_json(
            "POST",
            "/api/file-actions/move",
            &serde_json::json!({
                "collection_ids": [second_id],
                "archive_root_id": downloads_root_id
            }),
        )
        .await;
    assert_eq!(200, invalid_root.status);
    assert_eq!(1, invalid_root.json["failed"]);
    assert!(second_path.is_file());

    fs::remove_file(&second_path).expect("remove source before request");
    let missing_source = server
        .request_json(
            "POST",
            "/api/file-actions/move",
            &serde_json::json!({
                "collection_ids": [second_id],
                "archive_root_id": archive_root_id
            }),
        )
        .await;
    assert_eq!(200, missing_source.status);
    assert_eq!(1, missing_source.json["failed"]);
    assert_eq!("failed", missing_source.json["items"][0]["status"]);
    server.stop().await;
}

#[tokio::test]
async fn delete_api_requires_an_explicit_mode_and_supports_soft_and_permanent_delete() {
    let tree = TestTree::new("file-delete-api");
    let soft_name = "[circle] soft.zip";
    let permanent_name = "[circle] permanent.zip";
    tree.zip_in("downloads", soft_name);
    tree.zip_in("downloads", permanent_name);
    let downloads = tree.root("downloads");
    let soft_path = downloads.join(soft_name);
    let permanent_path = downloads.join(permanent_name);
    let recycle = tree.root("recycle");
    fs::create_dir(&recycle).expect("create fake recycle bin");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(
        repository,
        FakeRecycleBin {
            directory: recycle.clone(),
        },
    );
    application
        .run_scan(&[ScanRoot {
            path: downloads,
            source: SourceKind::Downloads,
            label: "下載區".to_owned(),
        }])
        .expect("scan downloads");
    let soft_id = application
        .repository()
        .collection_id_for_current_path(&soft_path)
        .expect("soft lookup")
        .expect("soft collection");
    let permanent_id = application
        .repository()
        .collection_id_for_current_path(&permanent_path)
        .expect("permanent lookup")
        .expect("permanent collection");
    let server = RunningServer::start(application).await;

    let missing_mode = server
        .request_json(
            "POST",
            "/api/file-actions/delete",
            &serde_json::json!({ "collection_ids": [soft_id] }),
        )
        .await;
    assert_eq!(400, missing_mode.status);
    assert_eq!("invalid_json", missing_mode.json["error"]["code"]);
    assert!(soft_path.is_file());

    let invalid_mode = server
        .request_json(
            "POST",
            "/api/file-actions/delete",
            &serde_json::json!({ "collection_ids": [soft_id], "mode": "hard" }),
        )
        .await;
    assert_eq!(400, invalid_mode.status);
    assert_eq!("invalid_delete_mode", invalid_mode.json["error"]["code"]);
    assert!(soft_path.is_file());

    let soft = server
        .request_json(
            "POST",
            "/api/file-actions/delete",
            &serde_json::json!({ "collection_ids": [soft_id], "mode": "soft" }),
        )
        .await;
    assert_eq!(200, soft.status);
    assert_eq!(1, soft.json["succeeded"]);
    assert_eq!("succeeded", soft.json["items"][0]["status"]);
    assert!(!soft_path.exists());
    assert!(recycle.join(soft_name).is_file());

    let permanent = server
        .request_json(
            "POST",
            "/api/file-actions/delete",
            &serde_json::json!({
                "collection_ids": [permanent_id],
                "mode": "permanent"
            }),
        )
        .await;
    assert_eq!(200, permanent.status);
    assert_eq!(1, permanent.json["succeeded"]);
    assert!(!permanent_path.exists());
    assert!(!recycle.join(permanent_name).exists());

    for collection_id in [soft_id, permanent_id] {
        let hidden = server
            .request("GET", &format!("/api/collections/{collection_id}"), &[])
            .await;
        assert_eq!(404, hidden.status);
    }
    server.stop().await;
}

#[tokio::test]
async fn cross_site_writes_are_rejected_without_substring_host_bypass() {
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;

    let rejected = server
        .request(
            "POST",
            "/api/scans",
            &[("Origin", "http://localhost.evil.example")],
        )
        .await;
    assert_eq!(403, rejected.status);
    assert_eq!("cross_site_write_rejected", rejected.json["error"]["code"]);

    let accepted = server
        .request("POST", "/api/scans", &[("Origin", "http://localhost:5000")])
        .await;
    assert_eq!(200, accepted.status);
    server.stop().await;
}

#[tokio::test]
async fn running_scan_conflict_and_unknown_scan_have_json_errors() {
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository.begin_scan_run().expect("mark running scan");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;

    let conflict = server.request("POST", "/api/scans", &[]).await;
    assert_eq!(409, conflict.status);
    assert_eq!("scan_already_running", conflict.json["error"]["code"]);

    let missing = server.request("GET", "/api/scans/999", &[]).await;
    assert_eq!(404, missing.status);
    assert_eq!("scan_run_not_found", missing.json["error"]["code"]);

    let invalid = server.request("GET", "/api/scans/not-a-number", &[]).await;
    assert_eq!(400, invalid.status);
    assert_eq!("invalid_scan_run_id", invalid.json["error"]["code"]);

    let unknown = server.request("GET", "/api/unknown", &[]).await;
    assert_eq!(404, unknown.status);
    assert_eq!("route_not_found", unknown.json["error"]["code"]);

    let wrong_method = server.request("DELETE", "/api/health", &[]).await;
    assert_eq!(405, wrong_method.status);
    assert_eq!("method_not_allowed", wrong_method.json["error"]["code"]);
    server.stop().await;
}
