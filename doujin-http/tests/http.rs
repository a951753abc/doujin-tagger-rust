use std::fs;
use std::io;
use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use doujin_app::{ApplicationService, ApplicationSettingsOverrides};
use doujin_files::{CollectionLauncher, RecycleBin};
use doujin_http::{
    EhentaiHttpServices, MagnetLauncher, SharedApplication, bind_loopback,
    serve_shared_with_shutdown, serve_shared_with_shutdown_and_ehentai, share_application,
    validate_loopback_address,
};
use doujin_parser::domain::{Authors, Parody};
use doujin_provider_ehentai::{
    CookieHeader, CookieStore, EhentaiSource, GalleryDetail, GallerySearchPage, GallerySearchQuery,
    GalleryTorrent, GalleryTorrentList, SessionStatus, SourceError, SourceErrorKind, SourceGallery,
    SourceSite, TorrentDownload,
};
use doujin_scanner::{ScanRoot, SourceKind};
use doujin_storage::CatalogRepository;
use doujin_storage::external_search_batches::ExternalSearchBatchStrategy;
use doujin_storage::jobs::{
    ExternalSearchCompletionStatus, ExternalSearchErrorKind, ExternalSearchJobIssue,
    ExternalSearchJobSnapshot, ExternalSearchJobSummary,
};
use doujin_storage::metadata::{
    ConfidenceEvidence, ExternalCandidate, ExternalCandidateOutcome, MetadataField, MetadataValue,
};
use doujin_storage::thumbnails::{ThumbnailErrorKind, ThumbnailStatus};
use doujin_thumbnails::{
    ThumbnailConfig, ThumbnailError, ThumbnailGenerationSuccess,
    calculate_source_content_fingerprint, transparent_placeholder_webp,
};
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use zip::write::SimpleFileOptions;

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

#[derive(Clone, Default)]
struct RecordingMagnetLauncher {
    calls: Arc<Mutex<Vec<String>>>,
    fail: bool,
}

impl RecordingMagnetLauncher {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("magnet calls").clone()
    }
}

impl MagnetLauncher for RecordingMagnetLauncher {
    fn open_magnet(&self, magnet_uri: &str) -> Result<(), String> {
        if self.fail {
            return Err("simulated failure".to_owned());
        }
        self.calls
            .lock()
            .expect("magnet calls")
            .push(magnet_uri.to_owned());
        Ok(())
    }
}

struct FakeEhentaiSource {
    cookies: CookieStore,
    error: Option<SourceErrorKind>,
}

impl FakeEhentaiSource {
    fn result<T>(&self, value: T) -> Result<T, SourceError> {
        match self.error {
            Some(kind) => Err(SourceError {
                kind,
                message: "fake source URL https://example.invalid/?secret=hidden".to_owned(),
            }),
            None => Ok(value),
        }
    }

    fn gallery_value(&self) -> SourceGallery {
        SourceGallery {
            source: SourceSite::Ehentai,
            gid: 123,
            token: "0123456789".to_owned(),
            title: "Romanized".to_owned(),
            title_jpn: Some("日本語".to_owned()),
            category: "Doujinshi".to_owned(),
            thumb: Some("https://ehgt.org/thumb.jpg".to_owned()),
            uploader: Some("alice".to_owned()),
            posted: Some("1700000000".to_owned()),
            rating: Some(4.75),
            tags: vec!["language:japanese".to_owned()],
            pages: Some(42),
        }
    }
}

impl EhentaiSource for FakeEhentaiSource {
    fn validate_session(&self) -> SessionStatus {
        if self.cookies.is_configured() {
            SessionStatus::Exhentai
        } else {
            SessionStatus::NotConfigured
        }
    }

    fn search(&self, query: &GallerySearchQuery) -> Result<GallerySearchPage, SourceError> {
        self.result(GallerySearchPage {
            source: SourceSite::Ehentai,
            page: query.page,
            has_next: true,
            next_cursor: query.cursor.clone(),
            previous_cursor: Some("prev:4127000".to_owned()),
            galleries: vec![self.gallery_value()],
        })
    }

    fn gallery(&self, _: u64, _: &str) -> Result<GalleryDetail, SourceError> {
        self.result(self.gallery_value())
    }

    fn torrents(&self, _: u64, _: &str) -> Result<GalleryTorrentList, SourceError> {
        self.result(GalleryTorrentList {
            source: SourceSite::Ehentai,
            torrents: vec![GalleryTorrent {
                name: "Fixture".to_owned(),
                posted_at: "2026-01-02T03:04:05Z".to_owned(),
                size: "12 MiB".to_owned(),
                seeds: 9,
                peers: 8,
                downloads: 7,
                outdated: false,
                torrent_url: "https://ehtracker.org/get/fixture".to_owned(),
                magnet_url: Some(format!("magnet:?xt=urn:btih:{}", "a".repeat(40))),
            }],
        })
    }

    fn download_torrent(&self, _: &str) -> Result<TorrentDownload, SourceError> {
        self.result(TorrentDownload {
            bytes: b"d4:infod4:name7:fixtureee".to_vec(),
            content_type: Some("application/x-bittorrent".to_owned()),
        })
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

    fn image_zip(&self, filename: &str, entries: &[(&str, [u8; 4])]) {
        let path = self.library().join(filename);
        fs::create_dir_all(path.parent().expect("zip parent")).expect("create library");
        let mut archive = zip::ZipWriter::new(fs::File::create(path).expect("create image ZIP"));
        for (entry, color) in entries {
            archive
                .start_file(*entry, SimpleFileOptions::default())
                .expect("start image entry");
            let image = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(24, 32, Rgba(*color)));
            let mut encoded = io::Cursor::new(Vec::new());
            image
                .write_to(&mut encoded, ImageFormat::Png)
                .expect("encode image entry");
            archive
                .write_all(&encoded.into_inner())
                .expect("write image entry");
        }
        archive.finish().expect("finish image ZIP");
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

    async fn start_with_ehentai<R>(
        application: ApplicationService<R>,
        ehentai: EhentaiHttpServices,
    ) -> Self
    where
        R: RecycleBin + Send + 'static,
    {
        let listener = bind_loopback(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind loopback");
        let address = listener.local_addr().expect("listener address");
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(serve_shared_with_shutdown_and_ehentai(
            listener,
            share_application(application),
            ehentai,
            async {
                let _ = receiver.await;
            },
        ));
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

fn external_search_test_application(
    tree: &TestTree,
    filenames: &[&str],
    mut repository: CatalogRepository,
) -> (ApplicationService<NoopRecycleBin>, Vec<i64>) {
    for filename in filenames {
        tree.zip(filename);
    }
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "歸檔區")
        .expect("register external search test root");
    let roots = repository
        .active_scan_roots()
        .expect("read external search test roots");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&roots)
        .expect("scan external search test collections");
    let collection_ids = filenames
        .iter()
        .map(|filename| {
            application
                .repository()
                .collection_id_for_current_path(&tree.library().join(filename))
                .expect("external search test collection lookup")
                .expect("external search test collection ID")
        })
        .collect();
    (application, collection_ids)
}

fn fail_external_search_job(
    mut application: ApplicationService<NoopRecycleBin>,
    collection_id: i64,
    fields: &[MetadataField],
    error_kind: ExternalSearchErrorKind,
    summary: Option<&ExternalSearchJobSummary>,
) -> (
    ApplicationService<NoopRecycleBin>,
    ExternalSearchJobSnapshot,
) {
    let job = application
        .enqueue_external_search(collection_id, fields)
        .expect("enqueue external search test job")
        .job;
    let mut repository = application.into_repository();
    repository
        .start_external_search_job(job.id)
        .expect("start external search test job");
    let failed = repository
        .fail_external_search_job(
            job.id,
            error_kind,
            "simulated external search failure",
            summary,
        )
        .expect("fail external search test job");
    (ApplicationService::new(repository, NoopRecycleBin), failed)
}

fn complete_partial_external_search_job(
    mut application: ApplicationService<NoopRecycleBin>,
    collection_id: i64,
    fields: &[MetadataField],
    summary: &ExternalSearchJobSummary,
) -> (
    ApplicationService<NoopRecycleBin>,
    ExternalSearchJobSnapshot,
) {
    let job = application
        .enqueue_external_search(collection_id, fields)
        .expect("enqueue partial external search test job")
        .job;
    let mut repository = application.into_repository();
    repository
        .start_external_search_job(job.id)
        .expect("start partial external search test job");
    let partial = repository
        .complete_external_search_job(job.id, ExternalSearchCompletionStatus::Partial, summary)
        .expect("complete partial external search test job");
    (ApplicationService::new(repository, NoopRecycleBin), partial)
}

fn external_search_activity_item(activity: &Value, job_id: i64) -> &Value {
    activity["items"]
        .as_array()
        .expect("activity items")
        .iter()
        .find(|item| item["id"].as_i64() == Some(job_id))
        .expect("external search activity item")
}

#[tokio::test]
async fn export_api_enforces_registered_roots_preflight_boundaries_and_completes_package() {
    let tree = TestTree::new("export-api");
    tree.zip("[Circle A] Book A.zip");
    tree.zip("[Circle B] Book B.zip");
    let export_directory = tree.root("exports");
    fs::create_dir_all(&export_directory).expect("export directory");
    let repository = CatalogRepository::open_in_memory().expect("catalog");
    let launcher = RecordingLauncher::new(false);
    let mut application =
        ApplicationService::with_launcher(repository, NoopRecycleBin, launcher.clone(), None);
    application
        .run_scan(&[ScanRoot {
            path: tree.library(),
            source: SourceKind::Downloads,
            label: "下載區".to_owned(),
        }])
        .expect("scan");
    let server = RunningServer::start(application).await;

    let empty = server.request("GET", "/api/export-roots", &[]).await;
    assert_eq!(200, empty.status);
    assert_eq!(0, empty.json["roots"].as_array().expect("roots").len());
    let registered = server
        .request_json(
            "POST",
            "/api/export-roots",
            &serde_json::json!({
                "path": export_directory.to_string_lossy(),
                "label": "外接硬碟"
            }),
        )
        .await;
    assert_eq!(200, registered.status);
    let root_id = registered.json["id"].as_i64().expect("root ID");
    assert_eq!("外接硬碟", registered.json["label"]);

    let relative = server
        .request_json(
            "POST",
            "/api/export-roots",
            &serde_json::json!({"path": "relative", "label": "unsafe"}),
        )
        .await;
    assert_eq!(400, relative.status);
    let unknown_root_field = server
        .request_json(
            "POST",
            "/api/export-roots",
            &serde_json::json!({
                "path": export_directory.to_string_lossy(),
                "label": "unsafe",
                "source": "archive"
            }),
        )
        .await;
    assert_eq!(400, unknown_root_field.status);

    let request = serde_json::json!({
        "collection_ids": [1, 2],
        "export_root_id": root_id,
        "package_filename": "C106:精選.zip"
    });
    let preflight = server
        .request_json("POST", "/api/export-jobs/preflight", &request)
        .await;
    assert_eq!(200, preflight.status);
    assert_eq!(2, preflight.json["selected"]);
    assert_eq!(2, preflight.json["exportable"]);
    assert_eq!("C106_精選.zip", preflight.json["package_filename"]);
    assert_eq!(true, preflight.json["can_start"]);
    assert_eq!(false, preflight.json["cancellation_supported"]);

    let raw_path_attempt = server
        .request_json(
            "POST",
            "/api/export-jobs",
            &serde_json::json!({
                "collection_ids": [1],
                "export_root_id": root_id,
                "package_filename": "safe.zip",
                "destination_path": export_directory.join("escape.zip").to_string_lossy()
            }),
        )
        .await;
    assert_eq!(400, raw_path_attempt.status);

    let collision_path = export_directory.join("C106_精選.zip");
    fs::write(&collision_path, b"existing").expect("collision");
    let collision = server
        .request_json("POST", "/api/export-jobs/preflight", &request)
        .await;
    assert_eq!(true, collision.json["package_collision"]);
    assert_eq!(false, collision.json["can_start"]);
    fs::remove_file(&collision_path).expect("remove collision");

    let missing_source = tree.library().join("[Circle B] Book B.zip");
    let saved_source = fs::read(&missing_source).expect("source bytes");
    fs::remove_file(&missing_source).expect("remove source");
    let missing = server
        .request_json("POST", "/api/export-jobs/preflight", &request)
        .await;
    assert_eq!(1, missing.json["missing"]);
    assert_eq!(false, missing.json["can_start"]);
    fs::write(&missing_source, saved_source).expect("restore source");

    let created = server
        .request_json("POST", "/api/export-jobs", &request)
        .await;
    assert_eq!(200, created.status);
    let job_id = created.json["id"].as_i64().expect("job ID");
    let completed = poll_export_job(&server, job_id).await;
    assert_eq!("succeeded", completed["status"]);
    assert_eq!(2, completed["processed_items"]);
    assert_eq!(2, completed["succeeded_items"]);
    assert!(completed["processed_bytes"].as_u64().expect("bytes") > 0);
    let output = export_directory.join("C106_精選.zip");
    assert!(output.is_file());
    assert!(!export_directory.join("C106_精選.zip.partial").exists());
    let archive = zip::ZipArchive::new(fs::File::open(output).expect("output")).expect("outer ZIP");
    assert_eq!(3, archive.len());

    let opened = server
        .request(
            "POST",
            &format!("/api/export-jobs/{job_id}/open-location"),
            &[],
        )
        .await;
    assert_eq!(200, opened.status);
    assert_eq!(
        fs::canonicalize(&export_directory).expect("canonical export directory"),
        launcher.calls()[0].path
    );
    let unknown = server.request("GET", "/api/export-jobs/999999", &[]).await;
    assert_eq!(404, unknown.status);
    let invalid_id = server.request("GET", "/api/export-jobs/0", &[]).await;
    assert_eq!(400, invalid_id.status);
    server.stop().await;
}

#[tokio::test]
async fn failed_export_job_cannot_open_and_retry_reuses_registered_job_safely() {
    let tree = TestTree::new("export-retry-api");
    tree.zip("Book.zip");
    let export_directory = tree.root("exports");
    fs::create_dir_all(&export_directory).expect("exports");
    let repository = CatalogRepository::open_in_memory().expect("catalog");
    let launcher = RecordingLauncher::new(false);
    let mut application =
        ApplicationService::with_launcher(repository, NoopRecycleBin, launcher.clone(), None);
    application
        .run_scan(&[ScanRoot {
            path: tree.library(),
            source: SourceKind::Downloads,
            label: "下載區".to_owned(),
        }])
        .expect("scan");
    let root = application
        .register_export_root(&export_directory, "匯出")
        .expect("root");
    let job = application
        .enqueue_export(&[1], root.id, "retry.zip")
        .expect("enqueue");
    application
        .fail_export(job.id, None, "模擬中斷")
        .expect("failed job");
    let server = RunningServer::start(application).await;

    let unopened = server
        .request(
            "POST",
            &format!("/api/export-jobs/{}/open-location", job.id),
            &[],
        )
        .await;
    assert_eq!(400, unopened.status);
    assert!(launcher.calls().is_empty());
    let retried = server
        .request("POST", &format!("/api/export-jobs/{}/retry", job.id), &[])
        .await;
    assert_eq!(200, retried.status);
    let completed = poll_export_job(&server, job.id).await;
    assert_eq!("succeeded", completed["status"]);
    assert_eq!(1, completed["attempts"]);
    assert!(export_directory.join("retry.zip").is_file());
    server.stop().await;
}

async fn poll_export_job(server: &RunningServer, job_id: i64) -> Value {
    for _ in 0..200 {
        let response = server
            .request("GET", &format!("/api/export-jobs/{job_id}"), &[])
            .await;
        assert_eq!(200, response.status);
        if matches!(
            response.json["status"].as_str(),
            Some("succeeded" | "failed")
        ) {
            return response.json;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("export job did not finish")
}

#[tokio::test]
async fn duplicate_api_has_persistent_jobs_candidates_filters_and_fingerprint_bound_decisions() {
    let tree = TestTree::new("duplicate-api");
    tree.image_zip(
        "[Circle] Exact Original.zip",
        &[
            ("001.png", [200, 20, 30, 255]),
            ("002.png", [30, 20, 200, 255]),
        ],
    );
    fs::copy(
        tree.library().join("[Circle] Exact Original.zip"),
        tree.library().join("[Circle] Exact Copy.zip"),
    )
    .expect("copy exact ZIP");
    tree.image_zip("[Other] Unique.zip", &[("001.png", [10, 160, 80, 255])]);
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[ScanRoot {
            path: tree.library(),
            source: SourceKind::Downloads,
            label: "下載區".to_owned(),
        }])
        .expect("scan duplicate fixtures");
    let shared = share_application(application);
    let server = RunningServer::start_shared(Arc::clone(&shared)).await;

    let started = server.request("POST", "/api/duplicate-jobs", &[]).await;
    assert_eq!(200, started.status);
    assert_eq!(3, started.json["total"]);
    assert_eq!(2, started.json["concurrency_limit"]);
    assert_eq!(Value::Null, started.json["estimated_seconds_remaining"]);
    let job_id = started.json["id"].as_i64().expect("job ID");

    loop {
        let request = shared
            .lock()
            .expect("application")
            .claim_duplicate_fingerprint()
            .expect("claim fingerprint");
        let Some(request) = request else { break };
        let result = calculate_source_content_fingerprint(&request.source_path);
        shared
            .lock()
            .expect("application")
            .finish_duplicate_fingerprint(&request, result)
            .expect("finish fingerprint");
    }
    let completed = server
        .request("GET", &format!("/api/duplicate-jobs/{job_id}"), &[])
        .await;
    assert_eq!(200, completed.status);
    assert_eq!("completed", completed.json["status"]);
    assert_eq!(3, completed.json["processed"]);

    let candidates = server.request("GET", "/api/duplicates", &[]).await;
    assert_eq!(200, candidates.status);
    assert_eq!(1, candidates.json["total"]);
    assert_eq!("exact", candidates.json["items"][0]["level"]);
    assert!(
        candidates.json["items"][0]["left"]["collection"]["path"]
            .as_str()
            .expect("left path")
            .ends_with(".zip")
    );
    assert_eq!(2, candidates.json["items"][0]["left"]["page_count"]);
    assert!(
        candidates.json["items"][0]["reasons"][0]
            .as_str()
            .expect("reason")
            .contains("SHA-256")
    );
    let exact = server
        .request("GET", "/api/duplicates?level=exact", &[])
        .await;
    assert_eq!(1, exact.json["total"]);
    let probable = server
        .request("GET", "/api/duplicates?level=probable", &[])
        .await;
    assert_eq!(0, probable.json["total"]);
    let invalid = server
        .request("GET", "/api/duplicates?level=tombstone", &[])
        .await;
    assert_eq!(400, invalid.status);
    assert_eq!("invalid_duplicate_level", invalid.json["error"]["code"]);

    let pair = &candidates.json["items"][0];
    let left_id = pair["left"]["collection"]["id"].as_i64().expect("left ID");
    let right_id = pair["right"]["collection"]["id"]
        .as_i64()
        .expect("right ID");
    let decision = serde_json::json!({
        "left_fingerprint_identity": pair["left"]["fingerprint_identity"],
        "right_fingerprint_identity": pair["right"]["fingerprint_identity"],
    });
    let confirmed = server
        .request_json(
            "POST",
            &format!("/api/duplicates/{left_id}/{right_id}/confirm"),
            &decision,
        )
        .await;
    assert_eq!(200, confirmed.status);
    assert_eq!("confirmed", confirmed.json["status"]);
    assert_eq!(
        true,
        server.request("GET", "/api/duplicates", &[]).await.json["items"][0]["reviewed"]
    );

    let stale = server
        .request_json(
            "POST",
            &format!("/api/duplicates/{left_id}/{right_id}/exclude"),
            &serde_json::json!({
                "left_fingerprint_identity": "stale",
                "right_fingerprint_identity": decision["right_fingerprint_identity"],
            }),
        )
        .await;
    assert_eq!(409, stale.status);
    let excluded = server
        .request_json(
            "POST",
            &format!("/api/duplicates/{left_id}/{right_id}/exclude"),
            &decision,
        )
        .await;
    assert_eq!(200, excluded.status);
    assert_eq!(
        0,
        server.request("GET", "/api/duplicates", &[]).await.json["total"]
    );

    // Duplicate review has no delete or consolidation route. File deletion stays
    // exclusively behind `/api/file-actions/delete`.
    assert_eq!(
        404,
        server
            .request(
                "POST",
                &format!("/api/duplicates/{left_id}/{right_id}/delete"),
                &[]
            )
            .await
            .status
    );
    server.stop().await;
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
    assert!(policy.contains("img-src 'self' data: https://ehgt.org https://*.ehgt.org"));
    let image_sources = policy
        .split(';')
        .map(str::trim)
        .find(|directive| directive.starts_with("img-src "))
        .expect("img-src directive");
    assert!(
        !image_sources
            .split_whitespace()
            .any(|source| source == "https:")
    );
    assert!(!policy.contains("example.com"));
    let document = String::from_utf8(index.body).expect("UTF-8 frontend document");
    assert!(document.contains("<html lang=\"zh-Hant\">"));
    assert!(document.contains("id=\"main-content\""));
    assert!(document.contains("aria-live=\"polite\""));
    assert!(document.contains("href=\"/assets/app.css?v=61\""));
    assert!(document.contains("src=\"/assets/app.js?v=61\" defer"));
    assert!(document.contains("id=\"duplicates-view\""));
    assert!(document.contains("id=\"start-duplicate-scan\""));
    assert!(document.contains("id=\"rename-preflight-form\""));
    assert!(document.contains("id=\"rename-preflight-items\""));
    assert!(document.contains("id=\"review-view\""));
    assert!(document.contains("data-route=\"review\""));
    assert!(document.contains("id=\"review-desk\""));
    assert!(document.contains("id=\"review-accept\""));
    assert!(document.contains("id=\"review-search\""));
    assert!(document.contains(
        "id=\"review-external-status\" role=\"status\" aria-live=\"polite\" aria-atomic=\"true\""
    ));
    assert!(document.contains("外部搜尋（品質審核搜尋目前問題欄位；partial 時再試一次）"));
    assert!(document.contains("id=\"library-scroll-sentinel\""));
    assert!(document.contains("id=\"library-load-more\""));
    assert!(document.contains("id=\"library-load-announcer\""));
    assert!(document.contains("id=\"scan-results-dialog\""));
    assert!(document.contains("id=\"thumbnail-cache-preflight-dialog\""));
    assert!(document.contains("id=\"thumbnail-cache-retry-failures\""));
    assert!(document.contains("id=\"edit-root-dialog\""));
    assert!(document.contains("id=\"root-rescan-note\""));
    assert!(document.contains("id=\"first-run\""));
    assert!(document.contains("id=\"first-run-form\""));
    assert!(document.contains("name=\"downloads_path\""));
    assert!(document.contains("name=\"archive_path\""));
    assert!(document.contains("name=\"reader_mode\""));
    assert!(document.contains("name=\"scan_now\""));
    assert!(document.contains("id=\"first-run-service\""));
    assert!(document.contains("稍後設定，先查看書架"));
    assert!(document.contains("id=\"viewer-path-override\""));
    assert!(document.contains("id=\"library-empty-heading\""));
    assert!(document.contains("id=\"library-empty-primary\""));
    assert!(document.contains("id=\"library-sort\""));
    assert!(document.contains("id=\"saved-view-dialog\""));
    assert!(document.contains("id=\"saved-view-rule-summary\""));
    assert!(document.contains("id=\"update-saved-view\""));
    assert!(document.contains("id=\"save-as-view\""));
    assert!(document.contains("id=\"rename-saved-view\""));
    assert!(document.contains("id=\"delete-saved-view\""));
    assert!(document.contains("id=\"edit-shelf-composition\""));
    assert!(document.contains("id=\"shelf-composition-dialog\""));
    assert!(document.contains("id=\"shelf-composition-list\""));
    assert!(document.contains("aria-label=\"首頁書牆順序\""));
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
    assert!(document.contains("id=\"prepare-external-batch\""));
    assert!(document.contains("id=\"external-batch-preflight\""));
    assert!(document.contains("只搜尋目前缺少欄位"));
    assert!(document.contains("沒有可靠估算，因此不顯示 ETA"));
    assert!(document.contains("id=\"shelf-view\""));
    assert!(document.contains("data-route=\"shelf\""));
    assert!(document.contains("id=\"workbench-view\""));
    assert!(document.contains("id=\"return-to-library-context\""));
    assert!(document.contains("id=\"vocabulary-heading\""));
    assert!(document.contains("03 / 名稱治理"));
    assert!(document.contains("id=\"vocabulary-field\""));
    assert!(document.contains("返回原本的藏書位置調整選取"));
    assert!(document.contains("id=\"focus-filter-dialog\""));
    assert!(document.contains("清除篩選並定位"));
    assert!(document.contains("id=\"metadata-evidence\""));
    assert!(document.contains("id=\"mobile-detail-dialog\""));
    assert!(document.contains("id=\"close-mobile-detail\""));
    assert!(document.contains("id=\"activity-trigger\""));
    assert!(document.contains("id=\"activity-panel\""));
    assert!(document.contains("id=\"activity-announcer\""));
    assert!(document.contains("id=\"shelf-composition\" aria-live=\"polite\""));
    assert!(document.contains("恢復預設首頁"));
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
    assert!(document.contains("id=\"archive-button\""));
    assert!(document.contains("id=\"archive-target-dialog\""));
    assert!(document.contains("id=\"archive-target-select\""));
    assert!(document.contains("id=\"archive-confirm-dialog\""));
    assert!(document.contains("id=\"default-archive-root\""));
    assert!(document.contains("id=\"selection-quick-archive\""));
    assert!(document.contains("id=\"quick-archive-dialog\""));
    assert!(document.contains("id=\"quick-archive-items\""));
    assert!(document.contains("id=\"quick-archive-submit\""));
    assert!(document.contains("下一本／上一本（藏書、待歸檔、品質審核通用）"));
    assert!(document.contains("切換目前查看收藏的批次選取"));
    assert!(!document.contains(">加標籤</a>"));
    assert!(!document.contains(">寫入手動值</a>"));
    assert!(document.contains("id=\"operations-view\""));
    assert!(document.contains("aria-label=\"整理台分頁\""));
    assert!(document.contains("data-route=\"vocabulary\""));
    assert!(document.contains("id=\"operations-count\""));
    assert!(document.contains("id=\"operations-total\""));
    assert!(document.contains("id=\"vocabulary-count\""));
    assert!(document.contains("id=\"triage-view\""));
    assert!(document.contains("data-view=\"triage\""));
    assert!(document.contains("data-route=\"triage\""));
    assert!(document.contains("id=\"triage-count\""));
    assert!(document.contains("id=\"triage-desk\""));
    assert!(document.contains("id=\"triage-sequence\""));
    assert!(document.contains("id=\"triage-status\""));
    assert!(document.contains("id=\"triage-destination-label\""));
    assert!(document.contains("id=\"triage-destination-path\""));
    assert!(document.contains("id=\"triage-quality-summary\""));
    assert!(document.contains("id=\"triage-archive\""));
    assert!(document.contains("id=\"triage-edit\""));
    assert!(document.contains("id=\"triage-search\""));
    assert!(document.contains("id=\"triage-skip\""));
    assert!(document.contains("id=\"triage-detail\""));
    assert!(document.contains("id=\"triage-previous\""));
    assert!(document.contains("id=\"triage-next\""));
    assert!(document.contains("id=\"triage-empty-message\""));
    assert!(document.contains("下載區沒有待歸檔的收藏"));
    assert!(document.contains("name=\"triage_auto_advance\""));
    assert!(document.contains("id=\"triage-auto-advance\""));
    assert!(document.contains("待歸檔：歸檔成功後自動前進下一本"));
    assert!(document.contains("本分頁主要動作：待歸檔＝歸檔到典藏庫，品質審核＝採用主要候選"));
    assert!(document.contains("待歸檔：打開完整 Detail"));
    assert!(document.contains("品質審核：拒絕主要候選"));
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
    assert!(stylesheet.contains(".enrichment-operation"));
    assert!(stylesheet.contains(".collection-window-spacer"));
    assert!(stylesheet.contains("contain: layout paint style"));
    assert!(stylesheet.contains(".scan-issue-list"));
    assert!(stylesheet.contains(".long-task-warning"));
    assert!(stylesheet.contains(".root-actions"));
    assert!(stylesheet.contains(".field-override-note"));
    assert!(stylesheet.contains(".empty-state-actions"));
    assert!(stylesheet.contains(".sort-control"));
    assert!(stylesheet.contains(".saved-view-context"));
    assert!(stylesheet.contains(".saved-view-rule-summary"));
    assert!(stylesheet.contains(".shelf-composition-dialog"));
    assert!(stylesheet.contains(".shelf-composition-row"));
    assert!(stylesheet.contains(".shelf-composition-row-controls"));
    assert!(stylesheet.contains("#shelf-composition-dialog"));
    assert!(stylesheet.contains("@media (max-width: 899px)"));
    assert!(stylesheet.contains(".missing-metadata-actions"));
    assert!(stylesheet.contains(".tag-suggestion-combobox"));
    assert!(stylesheet.contains(".review-desk"));
    assert!(stylesheet.contains(".vocabulary-group"));
    assert!(stylesheet.contains(".vocabulary-preflight-facts"));
    assert!(stylesheet.contains(".cover-candidate-gallery"));
    assert!(stylesheet.contains(".rename-workflow"));
    assert!(stylesheet.contains(".rename-change-item"));
    assert!(!stylesheet.contains("font-size: 0.6875rem;"));
    assert!(!stylesheet.contains("font-size: 0.625rem;"));
    assert!(!stylesheet.contains("font-size: 0.5625rem;"));
    assert!(!stylesheet.contains("color: var(--faint);"));
    assert!(stylesheet.starts_with("/* 1. Tokens */"));
    assert!(stylesheet.contains("/* 10. Responsive, input modality, and reduced motion */"));
    assert!(!stylesheet.contains("UI redesign v19"));
    assert!(!stylesheet.contains(".brand-mark"));
    assert!(stylesheet.contains("--fs-display:"));
    assert!(stylesheet.contains("--fs-caption:"));
    assert!(stylesheet.contains(".num {"));
    assert!(!stylesheet.contains("font-weight: 800"));
    assert!(!stylesheet.contains("font-size: clamp("));
    assert!(!stylesheet.contains("max-width: 1536px"));
    assert!(!stylesheet.contains("letter-spacing: 0.14em"));

    let javascript = server.request("GET", "/assets/app.js", &[]).await;
    assert_eq!(200, javascript.status);
    assert_eq!(
        Some("text/javascript; charset=utf-8"),
        javascript.header("content-type")
    );
    let script = String::from_utf8(javascript.body).expect("UTF-8 script");
    assert!(script.contains("doujin-library.recent.v1"));
    assert!(script.contains("const RECENT_LIMIT = 20"));
    assert!(script.contains("const DEFAULT_LIBRARY_BATCH_SIZE = 48"));
    assert!(
        script.contains("const LIBRARY_BATCH_SIZE_CHOICES = Object.freeze([24, 48, 96, 144, 192])")
    );
    assert!(script.contains("per_page: String(normalizeLibraryBatchSize(batchSize))"));
    assert!(script.contains("loadMoreCollections"));
    assert!(script.contains("if (libraryLoadPromise) return libraryLoadPromise"));
    assert!(script.contains("function moveLibraryFocus"));
    assert!(script.contains("button?.scrollIntoView({ block: \"nearest\" })"));
    assert!(script.contains("已顯示全部 ${formatNumber(state.total)} 本收藏"));
    assert!(script.contains(
        "已載入 ${formatNumber(additions.length)} 本，尚有 ${formatNumber(remaining)} 本"
    ));
    assert!(script.contains("rootMargin: \"1200px 0px\""));
    assert!(script.contains("/api/collections"));
    assert!(script.contains("/api/review-queue?kind=${encodeURIComponent(state.reviewKind)}"));
    assert!(script.contains("function decideReviewCandidate"));
    assert!(script.contains("function enqueueReviewExternalSearch"));
    assert!(script.contains("body: { fields: [issue.field] }"));
    assert!(script.contains("實際搜尋欄位：${actualFields}"));
    assert!(script.contains("function loadReviewExternalJob"));
    assert!(script.contains("preserveLiveContext: true"));
    assert!(script.contains("currentReviewItem()?.collection.id ?? preferredId"));
    assert!(script.contains("job?.status === \"partial\""));
    assert!(script.contains("!job || job.status === \"succeeded\""));
    assert!(script.contains("job.error_kind"));
    assert!(script.contains("job.next_retry_at"));
    assert!(script.contains("!ui.reviewSearch.disabled && !ui.reviewSearch.hidden"));
    assert!(script.contains("state.reviewSkipped.add(item.collection.id)"));
    assert!(script.contains("state.reviewReturnId = item.collection.id"));
    assert!(script.contains("target instanceof HTMLInputElement || target instanceof HTMLSelectElement || target instanceof HTMLTextAreaElement || target?.isContentEditable"));
    assert!(script.contains("!event.altKey && !event.ctrlKey && !event.metaKey"));
    assert!(script.contains("/api/saved-views"));
    assert!(script.contains("const SHELF_PREVIEW_LIMITS = Object.freeze([6, 8, 12, 16])"));
    assert!(script.contains("/api/shelf-configuration"));
    assert!(script.contains("function renderShelfComposition"));
    assert!(script.contains("function renderConfiguredShelf"));
    assert!(script.contains("section.setAttribute(\"aria-labelledby\", headingId)"));
    assert!(script.contains("檢視「${descriptor.title}」全部收藏"));
    assert!(script.contains("function reorderShelfItem"));
    assert!(script.contains("row.draggable = true"));
    assert!(script.contains("data-shelf-action"));
    assert!(script.contains("function savedViewShelfParams"));
    assert!(script.contains("downloads: \"新收藏\""));
    assert!(script.contains("archive: \"典藏庫\""));
    assert!(script.contains("function savedViewIsModified"));
    assert!(script.contains("function openSavedView"));
    assert!(script.contains("function updateActiveSavedView"));
    assert!(script.contains("function deleteActiveSavedView"));
    assert!(script.contains("params.set(\"view\", String(savedViewId))"));
    assert!(script.contains("rememberLaunch(state.selected, kind)"));
    assert!(script.contains("applyFilter(filterName, row.name)"));
    assert!(script.contains("/api/file-actions/move"));
    assert!(script.contains("/api/file-actions/rename/preflight"));
    assert!(script.contains("function renderRenamePreflight"));
    assert!(script.contains("expected_destination: item.expected_destination"));
    assert!(script.contains("/api/file-actions/delete"));
    assert!(script.contains("/api/tombstone-candidates"));
    assert!(script.contains("executeConsolidation"));
    assert!(script.contains("/api/vocabulary/candidates"));
    assert!(script.contains("/api/vocabulary/preflight"));
    assert!(script.contains("/api/vocabulary/merge"));
    assert!(script.contains("/api/vocabulary/reject"));
    assert!(script.contains("/api/duplicate-jobs"));
    assert!(script.contains("/api/duplicates"));
    assert!(script.contains("handoffDuplicateDelete"));
    assert!(script.contains("prepareDelete"));
    assert!(script.contains("function removeVocabularyVariant"));
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
    assert!(script.contains("function openCoverSelection"));
    assert!(script.contains("preview.loading = \"lazy\""));
    assert!(script.contains("aria-pressed"));
    assert!(script.contains("function clearCoverSelection"));
    assert!(script.contains("cover-candidates/preview?entry="));
    assert!(script.contains("requestFilterPanelClose({ restoreFocus: true })"));
    assert!(script.contains("updateSelectionCheckbox(selection"));
    assert!(script.contains("從批次選取移除"));
    assert!(script.contains("查看 ${displayTitle(collection)} 詳情"));
    assert!(!script.contains("`選取 ${displayTitle(collection)}`"));
    assert!(script.contains("已選 ${formatNumber(state.selectedIds.size)} 本 · 已載入 ${formatNumber(state.items.length)} 本 · 符合 ${formatNumber(state.total)} 本"));
    assert!(script.contains("function initializeTagSuggestionInputs"));
    assert!(script.contains("使用 ${formatNumber(option.count)} 次"));
    assert!(script.contains("controller.form?.requestSubmit()"));
    assert!(script.contains("function renderMissingMetadataActions"));
    assert!(script.contains("openMetadataDialog(field)"));
    assert!(script.contains("前往整理台處理 ${formatNumber(count)} 本"));
    assert!(script.contains("function selectionImpactSummary"));
    assert!(script.replace("\r\n", "\n").contains(
        "numSpan(formatNumber(unaffectedCount)),\n      document.createTextNode(\" 本不受影響。\"),"
    ));
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
    assert!(script.contains("function renderFirstRun"));
    assert!(script.contains("hasDownloads && hasArchive"));
    assert!(script.contains("async function completeFirstRun"));
    assert!(script.contains("/api/scans/preflight"));
    assert!(script.contains("viewer_path: settingsSnapshot?.overrides.viewer_path"));
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
    assert!(script.contains("function preflightExternalBatch"));
    assert!(script.contains("/api/external-search-batches/preflight"));
    assert!(script.contains("function renderExternalBatch"));
    assert!(script.contains("function retryExternalBatch"));
    assert!(script.contains("typed terminal failure"));
    assert!(script.contains("依 backoff 自動重試"));
    assert!(script.contains("前往品質審核"));
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
    assert!(script.contains("永久刪除 ${state.selectedIds.size} 本"));
    assert!(script.contains("/api/file-actions/move/preflight"));
    assert!(script.contains("async function resolveQuickArchiveTarget"));
    assert!(script.contains("default_archive_root_id"));
    assert!(script.contains("async function archiveSelectedToLibrary"));
    assert!(script.contains("async function executeArchiveToLibrary"));
    assert!(script.contains("function renderDefaultArchiveRootSelect"));
    assert!(script.contains("async function prepareQuickArchive"));
    assert!(script.contains("function openQuickArchiveDialog"));
    assert!(script.contains("async function executeQuickArchive"));
    assert!(script.contains(
        "body: { collection_ids: pending.readyIds, archive_root_id: pending.archiveRootId }"
    ));
    assert!(script.contains(
        "items.filter((item) => QUICK_ARCHIVE_READY_STATUSES.includes(item.status)).map((item) => item.collection_id)"
    ));
    assert!(script.contains("async function removeArchivedFromLibrary"));
    assert!(
        script.contains("state.route !== \"library\" || state.filters.source !== \"downloads\"")
    );
    assert!(
        script.contains("state.total = Math.max(0, (Number(state.total) || 0) - removal.size)")
    );
    assert!(
        script.contains(
            "if (focusRemoved) selectCollection(state.items[nextIndex], { focus: true })"
        )
    );
    assert!(script.contains("await removeArchivedFromLibrary([pending.collectionId])"));
    assert!(script.contains("快速歸檔 ${formatNumber(count)} 本"));
    assert!(stylesheet.contains(".quick-archive-item"));
    assert!(script.contains(
        "const skipped = items.filter((item) => !QUICK_ARCHIVE_READY_STATUSES.includes(item.status))"
    ));
    assert!(script.contains("state.quickArchivePreflight = { archiveRootId, readyIds, skipped }"));
    assert!(script.contains("async function applyQuickArchiveReport(report, skipped = [])"));
    assert!(script.contains("待復原 ${report.pending_recovery}、未執行 ${skipped.length}"));
    assert!(script.contains("batchResultItem(collection, \"skipped\", `未執行 · ${reason}`)"));
    assert!(stylesheet.contains(".batch-result .result-skipped span"));
    assert!(script.contains("state.requestNumber += 1;"));
    assert!(script.contains(
        "libraryLoadPromise.then(scheduleLibraryLoadCheck, scheduleLibraryLoadCheck)"
    ));
    assert!(script.contains("const current = await api(\"/api/settings\");"));
    assert!(!script.contains("state.settingsSnapshot || await api"));

    assert!(script.contains("doujin-library.triage-auto-advance.v1"));
    assert!(script.contains("const TRIAGE_PER_PAGE = 100"));
    assert!(script.contains("\"shelf\", \"library\", \"triage\", \"review\""));
    assert!(script.contains("triage: \"待歸檔\""));
    assert!(script.contains("if (state.route === \"triage\") enterTriage()"));
    assert!(script.contains("async function loadTriageQueue"));
    assert!(script.contains("source: \"downloads\","));
    assert!(script.contains("per_page: String(TRIAGE_PER_PAGE)"));
    assert!(script.contains("function updateTriageBadge"));
    assert!(script.contains("ui.triageCount.hidden = state.triageTotal === 0"));
    assert!(script.contains("function availableTriageIndices"));
    assert!(script.contains("async function ensureTriageArchiveRoot"));
    assert!(script.contains("state.triageArchiveRootId = await resolveQuickArchiveTarget()"));
    assert!(script.contains("async function refreshTriagePreflight"));
    assert!(script.contains(
        "body: { collection_ids: [collection.id], archive_root_id: state.triageArchiveRootId }"
    ));
    assert!(script.contains("function renderTriageReadiness"));
    assert!(script.contains("可直接歸檔，目的地如下。"));
    assert!(script.contains("可歸檔，但分類資料不足，將進未分類。"));
    assert!(script.contains("ui.triageArchive.disabled = !ready || state.triageArchiving"));
    assert!(script.contains("async function archiveCurrentTriageItem"));
    assert!(script.contains("await removeArchivedFromLibrary([collection.id])"));
    assert!(script.contains("function removeTriageItem"));
    assert!(script.contains("if (state.triageAutoAdvance && !ui.triageDesk.hidden)"));
    assert!(script.contains("function removeTriageItem(collectionId, { advance = true } = {})"));
    assert!(script.contains("removeTriageItem(collection.id, { advance: false })"));
    assert!(script.contains("function renderTriageArchivedResult"));
    assert!(script.contains("function clearTriageArchivedResult"));
    assert!(script.contains("function setTriageItemActionsEnabled"));
    assert!(script.contains("已歸檔這本，按 J 或「下一本」再處理下一本。"));
    assert!(script.contains("state.triageSkipped.add(collection.id)"));
    assert!(script.contains("state.triageReturnId = collection.id"));
    assert!(script.contains("function moveTriagePosition"));
    assert!(script.contains("async function enqueueTriageExternalSearch"));
    assert!(script.contains("function externalSearchFields"));
    assert!(script.contains("state.triageAutoAdvance = ui.triageAutoAdvance.checked"));
    assert!(script.contains("writeStorage(TRIAGE_AUTO_ADVANCE_KEY, state.triageAutoAdvance)"));
    assert!(script.contains(
        "state.route === \"triage\" && !isTyping && !isDialogOpen() && !event.altKey"
    ));
    assert!(script.contains("[\"a\", \"e\", \"w\", \"s\", \"j\", \"k\", \"o\"].includes(key)"));
    assert!(stylesheet.contains(".triage-desk"));
    assert!(stylesheet.contains(".triage-status-badge"));
    assert!(stylesheet.contains(".triage-readiness"));

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
async fn saved_views_crud_validates_allowlisted_rules_and_recounts_current_catalog() {
    let tree = TestTree::new("saved-views");
    tree.zip_in("downloads", "(C106) [AlphaCircle] First Story.zip");
    tree.zip_in("downloads", "(C106) [BetaCircle] Second Story.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.root("downloads"), SourceKind::Downloads, "下載區")
        .expect("register downloads");
    let roots = repository.active_scan_roots().expect("active roots");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&roots).expect("scan collections");
    let first_id = application
        .repository()
        .collection_id_for_current_path(
            &tree
                .root("downloads")
                .join("(C106) [AlphaCircle] First Story.zip"),
        )
        .expect("first lookup")
        .expect("first ID");
    let second_id = application
        .repository()
        .collection_id_for_current_path(
            &tree
                .root("downloads")
                .join("(C106) [BetaCircle] Second Story.zip"),
        )
        .expect("second lookup")
        .expect("second ID");
    application
        .add_collection_tag(first_id, "favorite")
        .expect("first favorite");
    application
        .add_collection_tag(first_id, "color")
        .expect("first color");
    application
        .add_collection_tag(second_id, "favorite")
        .expect("second favorite");
    let server = RunningServer::start(application).await;

    let payload = serde_json::json!({
        "name": "C106 待整理",
        "pinned": true,
        "query": {
            "q": "Story",
            "source": "downloads",
            "event": "C106",
            "tag": ["favorite", "color"],
            "missing": [],
            "untagged": false,
            "sort": "updated",
            "direction": "asc",
            "layout": "list"
        }
    });
    let created = server
        .request_json("POST", "/api/saved-views", &payload)
        .await;
    assert_eq!(201, created.status);
    assert_eq!("C106 待整理", created.json["name"]);
    assert_eq!(1, created.json["result_count"]);
    assert_eq!("downloads", created.json["query"]["source"]);
    assert_eq!("updated", created.json["query"]["sort"]);
    assert_eq!("list", created.json["query"]["layout"]);
    assert_eq!(
        Value::Null,
        created.json["query"]
            .get("focus")
            .cloned()
            .unwrap_or(Value::Null)
    );
    let saved_view_id = created.json["id"].as_i64().expect("saved view ID");

    let listed = server.request("GET", "/api/saved-views", &[]).await;
    assert_eq!(200, listed.status);
    assert_eq!(1, listed.json["items"].as_array().expect("views").len());

    let add_second_color = server
        .request_json(
            "POST",
            &format!("/api/collections/{second_id}/tags"),
            &serde_json::json!({ "name": "color" }),
        )
        .await;
    assert_eq!(200, add_second_color.status);
    let recounted = server
        .request("GET", &format!("/api/saved-views/{saved_view_id}"), &[])
        .await;
    assert_eq!(2, recounted.json["result_count"]);
    assert_eq!(Value::Null, recounted.json["query"]["per_page"]);

    let renamed = server
        .request_json(
            "PUT",
            &format!("/api/saved-views/{saved_view_id}"),
            &serde_json::json!({
                "name": "C106 雙標籤",
                "pinned": false,
                "query": payload["query"].clone()
            }),
        )
        .await;
    assert_eq!(200, renamed.status);
    assert_eq!("C106 雙標籤", renamed.json["name"]);
    assert_eq!(false, renamed.json["pinned"]);

    let duplicate = server
        .request_json(
            "POST",
            "/api/saved-views",
            &serde_json::json!({
                "name": "c106 雙標籤",
                "query": payload["query"].clone()
            }),
        )
        .await;
    assert_eq!(409, duplicate.status);
    assert_eq!("saved_view_name_conflict", duplicate.json["error"]["code"]);

    for invalid_query in [
        serde_json::json!({ "name": "SQL", "query": { "sort": "created", "direction": "desc", "layout": "grid", "where": "1=1" } }),
        serde_json::json!({ "name": "Sort", "query": { "sort": "random", "direction": "desc", "layout": "grid" } }),
        serde_json::json!({ "name": "Source", "query": { "sort": "created", "direction": "desc", "layout": "grid", "source": "remote" } }),
        serde_json::json!({ "name": "Layout", "query": { "sort": "created", "direction": "desc", "layout": "cards" } }),
    ] {
        let invalid = server
            .request_json("POST", "/api/saved-views", &invalid_query)
            .await;
        assert_eq!(400, invalid.status);
    }

    let deleted = server
        .request("DELETE", &format!("/api/saved-views/{saved_view_id}"), &[])
        .await;
    assert_eq!(204, deleted.status);
    let missing = server
        .request("GET", &format!("/api/saved-views/{saved_view_id}"), &[])
        .await;
    assert_eq!(404, missing.status);
    assert_eq!("saved_view_not_found", missing.json["error"]["code"]);
    server.stop().await;
}

#[tokio::test]
async fn shelf_configuration_is_validated_persisted_reset_and_cleaned_up_with_saved_views() {
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;

    let defaults = server.request("GET", "/api/shelf-configuration", &[]).await;
    assert_eq!(200, defaults.status);
    assert_eq!(
        3,
        defaults.json["items"]
            .as_array()
            .expect("default items")
            .len()
    );
    assert_eq!("recent", defaults.json["items"][0]["shelf_type"]);
    assert_eq!(8, defaults.json["items"][0]["preview_limit"]);

    let saved = server
        .request_json(
            "POST",
            "/api/saved-views",
            &serde_json::json!({
                "name": "首頁待整理",
                "pinned": true,
                "query": {
                    "missing": ["any"], "untagged": false,
                    "sort": "created", "direction": "desc", "layout": "grid"
                }
            }),
        )
        .await;
    assert_eq!(201, saved.status);
    let saved_view_id = saved.json["id"].as_i64().expect("saved view ID");
    let custom = serde_json::json!({
        "items": [
            {"shelf_type":"saved_view","saved_view_id":saved_view_id,"position":0,"enabled":true,"preview_limit":12},
            {"shelf_type":"recent","saved_view_id":null,"position":1,"enabled":false,"preview_limit":6},
            {"shelf_type":"featured","saved_view_id":null,"position":2,"enabled":true,"preview_limit":16},
            {"shelf_type":"event","saved_view_id":null,"position":3,"enabled":true,"preview_limit":8}
        ]
    });
    let replaced = server
        .request_json("PUT", "/api/shelf-configuration", &custom)
        .await;
    assert_eq!(200, replaced.status);
    assert_eq!(false, replaced.json["items"][1]["enabled"]);
    assert_eq!(12, replaced.json["items"][0]["preview_limit"]);

    let invalid = server
        .request_json(
            "PUT",
            "/api/shelf-configuration",
            &serde_json::json!({
                "items": [
                    {"shelf_type":"recent","saved_view_id":null,"position":0,"enabled":true,"preview_limit":8},
                    {"shelf_type":"featured","saved_view_id":null,"position":1,"enabled":true,"preview_limit":8},
                    {"shelf_type":"event","saved_view_id":null,"position":3,"enabled":true,"preview_limit":8}
                ]
            }),
        )
        .await;
    assert_eq!(400, invalid.status);
    let unchanged = server.request("GET", "/api/shelf-configuration", &[]).await;
    assert_eq!(custom["items"], unchanged.json["items"]);

    let mut invalid_preview = custom.clone();
    invalid_preview["items"][0]["preview_limit"] = serde_json::json!(10);
    let invalid_preview = server
        .request_json("PUT", "/api/shelf-configuration", &invalid_preview)
        .await;
    assert_eq!(400, invalid_preview.status);
    assert_eq!(
        "invalid_shelf_configuration",
        invalid_preview.json["error"]["code"]
    );
    let unchanged = server.request("GET", "/api/shelf-configuration", &[]).await;
    assert_eq!(custom["items"], unchanged.json["items"]);

    let reset = server
        .request("POST", "/api/shelf-configuration/reset", &[])
        .await;
    assert_eq!(200, reset.status);
    assert_eq!(
        3,
        reset.json["items"].as_array().expect("reset items").len()
    );
    let retained = server
        .request("GET", &format!("/api/saved-views/{saved_view_id}"), &[])
        .await;
    assert_eq!(200, retained.status);

    let restored = server
        .request_json("PUT", "/api/shelf-configuration", &custom)
        .await;
    assert_eq!(200, restored.status);
    let deleted = server
        .request("DELETE", &format!("/api/saved-views/{saved_view_id}"), &[])
        .await;
    assert_eq!(204, deleted.status);
    let cleaned = server.request("GET", "/api/shelf-configuration", &[]).await;
    assert!(
        cleaned.json["items"]
            .as_array()
            .expect("cleaned shelves")
            .iter()
            .all(|item| item["saved_view_id"].is_null())
    );
    assert_eq!(0, cleaned.json["items"][0]["position"]);
    assert_eq!(1, cleaned.json["items"][1]["position"]);
    assert_eq!(2, cleaned.json["items"][2]["position"]);
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
async fn review_queue_is_allowlisted_and_reuses_existing_metadata_decision_contracts() {
    let tree = TestTree::new("review-queue");
    for filename in [
        "[Missing] missing.zip",
        "[Accept] accept.zip",
        "[Reject] reject.zip",
        "[Clean] clean.zip",
    ] {
        tree.zip(filename);
    }
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "歸檔區")
        .expect("register root");
    let roots = repository.active_scan_roots().expect("active roots");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&roots).expect("scan review fixtures");
    let mut repository = application.into_repository();
    let id = |repository: &CatalogRepository, filename: &str| {
        repository
            .collection_id_for_current_path(&tree.library().join(filename))
            .expect("collection lookup")
            .expect("collection ID")
    };
    let missing_id = id(&repository, "[Missing] missing.zip");
    let accept_id = id(&repository, "[Accept] accept.zip");
    let reject_id = id(&repository, "[Reject] reject.zip");
    let clean_id = id(&repository, "[Clean] clean.zip");
    for collection_id in [missing_id, accept_id, reject_id, clean_id] {
        for (field, value) in [
            (
                MetadataField::Circle,
                MetadataValue::Text("review circle".to_owned()),
            ),
            (
                MetadataField::Authors,
                MetadataValue::Authors(doujin_parser::domain::Authors {
                    raw: Some("review author".to_owned()),
                    values: vec!["review author".to_owned()],
                }),
            ),
            (
                MetadataField::Parody,
                MetadataValue::Parody(doujin_parser::domain::Parody {
                    raw: "review parody".to_owned(),
                    canonical: "review parody".to_owned(),
                    evidence: "HTTP review fixture".to_owned(),
                }),
            ),
            (
                MetadataField::Classification,
                MetadataValue::Classification(doujin_parser::domain::Classification {
                    top_level: "同人誌".to_owned(),
                    subcategory: None,
                    raw_marker: None,
                }),
            ),
        ] {
            repository
                .set_manual_value(collection_id, field, value)
                .expect("complete shared review metadata");
        }
        if collection_id != missing_id {
            repository
                .set_manual_value(
                    collection_id,
                    MetadataField::Event,
                    MetadataValue::Text("C106".to_owned()),
                )
                .expect("complete review event");
        }
    }
    let candidate = |repository: &mut CatalogRepository, collection_id, value: &str| {
        let outcome = repository
            .save_external_candidate(ExternalCandidate {
                collection_id,
                field: MetadataField::Title,
                value: MetadataValue::Text(value.to_owned()),
                source_reference: format!("provider:review-{collection_id}"),
                confidence: confidence(0.8),
            })
            .expect("save review candidate");
        let ExternalCandidateOutcome::Suggestion { assertion_id, .. } = outcome else {
            panic!("review candidate must require a decision");
        };
        assertion_id
    };
    let accept_assertion = candidate(&mut repository, accept_id, "accepted review title");
    let reject_assertion = candidate(&mut repository, reject_id, "rejected review title");
    let server = RunningServer::start(ApplicationService::new(repository, NoopRecycleBin)).await;

    let listed = server
        .request("GET", "/api/review-queue?kind=all&page=1&per_page=2", &[])
        .await;
    assert_eq!(200, listed.status);
    assert_eq!(3, listed.json["pagination"]["total"]);
    assert_eq!(
        2,
        listed.json["items"].as_array().expect("review items").len()
    );
    assert_eq!(accept_id, listed.json["items"][0]["collection"]["id"]);
    let title = listed.json["items"][0]["metadata"]["fields"]
        .as_array()
        .expect("metadata fields")
        .iter()
        .find(|field| field["field"] == "title")
        .expect("title history");
    let assertion = title["assertions"]
        .as_array()
        .expect("title assertions")
        .iter()
        .find(|assertion| assertion["id"] == accept_assertion)
        .expect("review assertion");
    assert_eq!("candidate", assertion["status"]);
    assert_eq!(0.8, assertion["confidence_total"]);
    assert_eq!("HTTP metadata history test", assertion["reason"]);

    for path in [
        "/api/review-queue?kind=confidence",
        "/api/review-queue?kind=missing&kind=candidate",
        "/api/review-queue?unknown=all",
    ] {
        let invalid = server.request("GET", path, &[]).await;
        assert_eq!(400, invalid.status);
        assert_eq!("invalid_review_query", invalid.json["error"]["code"]);
    }

    let select = server
        .request_json(
            "PATCH",
            &format!("/api/collections/{accept_id}/metadata/title/assertions/{accept_assertion}"),
            &serde_json::json!({"decision": "select"}),
        )
        .await;
    assert_eq!(200, select.status);
    let reject = server
        .request_json(
            "PATCH",
            &format!("/api/collections/{reject_id}/metadata/title/assertions/{reject_assertion}"),
            &serde_json::json!({"decision": "reject"}),
        )
        .await;
    assert_eq!(200, reject.status);
    let manual = server
        .request_json(
            "PUT",
            &format!("/api/collections/{missing_id}/metadata/event"),
            &serde_json::json!({"value": "C106"}),
        )
        .await;
    assert_eq!(200, manual.status);

    let empty = server
        .request("GET", "/api/review-queue?kind=all", &[])
        .await;
    assert_eq!(0, empty.json["pagination"]["total"]);
    assert_eq!(
        0,
        empty.json["items"].as_array().expect("empty queue").len()
    );
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
async fn external_search_activity_auto_resolves_no_match_after_requested_metadata_changes() {
    let tree = TestTree::new("external-activity-auto-resolution");
    let (application, collection_ids) = external_search_test_application(
        &tree,
        &["[Circle] Missing Author.zip"],
        CatalogRepository::open_in_memory().expect("open catalog"),
    );
    let collection_id = collection_ids[0];
    let (application, failed_job) = fail_external_search_job(
        application,
        collection_id,
        &[MetadataField::Authors],
        ExternalSearchErrorKind::NoMatch,
        None,
    );
    let original_status = failed_job.status;
    let original_error = failed_job.error_message.clone();
    let original_attempts = failed_job.attempts;
    let server = RunningServer::start(application).await;

    let before = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, before.status);
    assert_eq!(1, before.json["actionable_count"]);
    let before_item = external_search_activity_item(&before.json, failed_job.id);
    assert_eq!(true, before_item["actionable"]);
    assert_eq!(Value::Null, before_item["resolution"]);
    assert_eq!(
        serde_json::json!(["authors"]),
        before_item["unresolved_fields"]
    );

    let manual = server
        .request_json(
            "PUT",
            &format!("/api/collections/{collection_id}/metadata/authors"),
            &serde_json::json!({"value": ["Manual Author"]}),
        )
        .await;
    assert_eq!(200, manual.status);

    let after = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, after.status);
    assert_eq!(0, after.json["actionable_count"]);
    let after_item = external_search_activity_item(&after.json, failed_job.id);
    assert_eq!(false, after_item["actionable"]);
    assert_eq!("metadata_resolved", after_item["resolution"]);
    assert_eq!(serde_json::json!([]), after_item["unresolved_fields"]);

    let preserved = server
        .request(
            "GET",
            &format!("/api/external-search-jobs/{}", failed_job.id),
            &[],
        )
        .await;
    assert_eq!(200, preserved.status);
    assert_eq!(original_status.as_str(), preserved.json["status"]);
    assert_eq!(
        original_error,
        preserved.json["error_message"].as_str().map(str::to_owned)
    );
    assert_eq!(original_attempts, preserved.json["attempts"]);
    server.stop().await;
}

#[tokio::test]
async fn external_search_activity_requires_post_enqueue_metadata_change_for_auto_resolution() {
    let tree = TestTree::new("external-activity-enqueue-baseline");
    let (mut application, collection_ids) = external_search_test_application(
        &tree,
        &["[Circle] Existing Author.zip"],
        CatalogRepository::open_in_memory().expect("open catalog"),
    );
    let collection_id = collection_ids[0];
    application
        .set_manual_metadata(
            collection_id,
            MetadataField::Authors,
            MetadataValue::Authors(Authors {
                raw: Some("Existing Author".to_owned()),
                values: vec!["Existing Author".to_owned()],
            }),
        )
        .expect("seed pre-enqueue authors");
    let (application, failed_job) = fail_external_search_job(
        application,
        collection_id,
        &[MetadataField::Authors],
        ExternalSearchErrorKind::NoMatch,
        None,
    );
    let server = RunningServer::start(application).await;

    let unchanged = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, unchanged.status);
    assert_eq!(1, unchanged.json["actionable_count"]);
    let unchanged_item = external_search_activity_item(&unchanged.json, failed_job.id);
    assert_eq!(true, unchanged_item["actionable"]);
    assert_eq!(Value::Null, unchanged_item["resolution"]);
    assert_eq!(
        serde_json::json!(["authors"]),
        unchanged_item["unresolved_fields"]
    );

    let replacement = server
        .request_json(
            "PUT",
            &format!("/api/collections/{collection_id}/metadata/authors"),
            &serde_json::json!({"value": ["Replacement Author"]}),
        )
        .await;
    assert_eq!(200, replacement.status);
    let changed = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(0, changed.json["actionable_count"]);
    assert_eq!(
        "metadata_resolved",
        external_search_activity_item(&changed.json, failed_job.id)["resolution"]
    );
    server.stop().await;
}

#[tokio::test]
async fn external_search_activity_resolves_partial_only_after_all_failed_fields_change() {
    let tree = TestTree::new("external-activity-partial-fields");
    let (application, collection_ids) = external_search_test_application(
        &tree,
        &["[Circle] Partial Fields.zip"],
        CatalogRepository::open_in_memory().expect("open catalog"),
    );
    let collection_id = collection_ids[0];
    let summary = ExternalSearchJobSummary {
        candidates_received: 0,
        tags_received: 0,
        tags_applied: 0,
        auto_applied: 0,
        suggestions: 0,
        suggestion_assertion_ids: Vec::new(),
        search_only: 0,
        issues: vec![
            ExternalSearchJobIssue {
                field: Some(MetadataField::Authors),
                kind: "provider_field_error".to_owned(),
                message: "authors lookup failed".to_owned(),
            },
            ExternalSearchJobIssue {
                field: Some(MetadataField::Parody),
                kind: "provider_field_error".to_owned(),
                message: "parody lookup failed".to_owned(),
            },
        ],
    };
    let (application, partial_job) = complete_partial_external_search_job(
        application,
        collection_id,
        &[MetadataField::Authors, MetadataField::Parody],
        &summary,
    );
    let server = RunningServer::start(application).await;

    let before = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, before.status);
    assert_eq!(1, before.json["actionable_count"]);
    assert_eq!(
        serde_json::json!(["authors", "parody"]),
        external_search_activity_item(&before.json, partial_job.id)["unresolved_fields"]
    );

    let authors = server
        .request_json(
            "PUT",
            &format!("/api/collections/{collection_id}/metadata/authors"),
            &serde_json::json!({"value": ["Manual Author"]}),
        )
        .await;
    assert_eq!(200, authors.status);
    let halfway = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(1, halfway.json["actionable_count"]);
    let halfway_item = external_search_activity_item(&halfway.json, partial_job.id);
    assert_eq!(true, halfway_item["actionable"]);
    assert_eq!(
        serde_json::json!(["parody"]),
        halfway_item["unresolved_fields"]
    );

    let parody = server
        .request_json(
            "PUT",
            &format!("/api/collections/{collection_id}/metadata/parody"),
            &serde_json::json!({"value": "Manual Parody"}),
        )
        .await;
    assert_eq!(200, parody.status);
    let complete = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(0, complete.json["actionable_count"]);
    let complete_item = external_search_activity_item(&complete.json, partial_job.id);
    assert_eq!(false, complete_item["actionable"]);
    assert_eq!("metadata_resolved", complete_item["resolution"]);
    assert_eq!(serde_json::json!([]), complete_item["unresolved_fields"]);
    server.stop().await;
}

#[tokio::test]
async fn external_search_activity_keeps_suggestions_and_system_failures_actionable() {
    let tree = TestTree::new("external-activity-conservative-resolution");
    let (application, collection_ids) = external_search_test_application(
        &tree,
        &["[Circle] Candidate.zip", "[Circle] System Error.zip"],
        CatalogRepository::open_in_memory().expect("open catalog"),
    );
    let suggestion_summary = ExternalSearchJobSummary {
        candidates_received: 1,
        tags_received: 0,
        tags_applied: 0,
        auto_applied: 0,
        suggestions: 1,
        suggestion_assertion_ids: Vec::new(),
        search_only: 0,
        issues: vec![ExternalSearchJobIssue {
            field: Some(MetadataField::Authors),
            kind: "provider_field_error".to_owned(),
            message: "authors lookup failed".to_owned(),
        }],
    };
    let (mut application, suggestion_job) = complete_partial_external_search_job(
        application,
        collection_ids[0],
        &[MetadataField::Authors],
        &suggestion_summary,
    );
    application
        .set_manual_metadata(
            collection_ids[0],
            MetadataField::Authors,
            MetadataValue::Authors(Authors {
                raw: Some("Manual Candidate Review".to_owned()),
                values: vec!["Manual Candidate Review".to_owned()],
            }),
        )
        .expect("resolve suggestion field manually");
    let (mut application, system_job) = fail_external_search_job(
        application,
        collection_ids[1],
        &[MetadataField::Authors],
        ExternalSearchErrorKind::InvalidResponse,
        None,
    );
    application
        .set_manual_metadata(
            collection_ids[1],
            MetadataField::Authors,
            MetadataValue::Authors(Authors {
                raw: Some("Manual System Recovery".to_owned()),
                values: vec!["Manual System Recovery".to_owned()],
            }),
        )
        .expect("fill system failure field manually");
    let server = RunningServer::start(application).await;

    let activity = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, activity.status);
    assert_eq!(2, activity.json["actionable_count"]);
    for job_id in [suggestion_job.id, system_job.id] {
        let item = external_search_activity_item(&activity.json, job_id);
        assert_eq!(true, item["actionable"]);
        assert_eq!(Value::Null, item["resolution"]);
    }
    server.stop().await;
}

#[tokio::test]
async fn external_search_acknowledgement_persists_without_rewriting_job_metadata_or_batch_history()
{
    let tree = TestTree::new("external-activity-ack-persistence");
    let database_path = tree.path.join("catalog.db");
    let (mut application, collection_ids) = external_search_test_application(
        &tree,
        &["[Circle] Acknowledged.zip"],
        CatalogRepository::open(&database_path).expect("open persistent catalog"),
    );
    let collection_id = collection_ids[0];
    let batch = application
        .create_external_search_batch(
            &[collection_id],
            &[MetadataField::Authors],
            ExternalSearchBatchStrategy::Specified,
        )
        .expect("create acknowledgement batch");
    let job_id = batch.items[0].job_id.expect("batch external search job");
    let mut repository = application.into_repository();
    repository
        .start_external_search_job(job_id)
        .expect("start acknowledgement job");
    repository
        .fail_external_search_job(
            job_id,
            ExternalSearchErrorKind::Unsupported,
            "provider does not support this query",
            None,
        )
        .expect("fail acknowledgement job");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;

    let job_before = server
        .request("GET", &format!("/api/external-search-jobs/{job_id}"), &[])
        .await;
    let metadata_before = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/metadata"),
            &[],
        )
        .await;
    let batch_before = server
        .request(
            "GET",
            &format!("/api/external-search-batches/{}", batch.id),
            &[],
        )
        .await;
    assert_eq!("failed", job_before.json["status"]);
    assert_eq!(1, batch_before.json["summary"]["failed"]);

    let acknowledged = server
        .request(
            "POST",
            &format!("/api/external-search-jobs/{job_id}/acknowledge"),
            &[],
        )
        .await;
    assert_eq!(200, acknowledged.status);
    assert_eq!(false, acknowledged.json["actionable"]);
    assert_eq!("acknowledged", acknowledged.json["resolution"]);
    assert!(acknowledged.json["acknowledged_at"].as_str().is_some());

    let job_after = server
        .request("GET", &format!("/api/external-search-jobs/{job_id}"), &[])
        .await;
    let metadata_after = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/metadata"),
            &[],
        )
        .await;
    let batch_after = server
        .request(
            "GET",
            &format!("/api/external-search-batches/{}", batch.id),
            &[],
        )
        .await;
    for history_field in [
        "id",
        "collection_id",
        "status",
        "fields",
        "result",
        "error_kind",
        "error_message",
        "attempts",
        "next_retry_at",
        "created_at",
        "updated_at",
    ] {
        assert_eq!(
            job_before.json[history_field], job_after.json[history_field],
            "acknowledgement must preserve job history field {history_field}"
        );
    }
    assert_eq!(metadata_before.json, metadata_after.json);
    assert_eq!(batch_before.json, batch_after.json);
    server.stop().await;

    let restarted = ApplicationService::new(
        CatalogRepository::open(&database_path).expect("reopen acknowledged catalog"),
        NoopRecycleBin,
    );
    let restarted_server = RunningServer::start(restarted).await;
    let persisted = restarted_server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, persisted.status);
    assert_eq!(0, persisted.json["actionable_count"]);
    let persisted_item = external_search_activity_item(&persisted.json, job_id);
    assert_eq!(false, persisted_item["actionable"]);
    assert_eq!("acknowledged", persisted_item["resolution"]);
    assert!(persisted_item["acknowledged_at"].as_str().is_some());
    restarted_server.stop().await;
}

#[tokio::test]
async fn external_search_acknowledgement_is_per_job_and_does_not_hide_a_new_failure() {
    let tree = TestTree::new("external-activity-ack-per-job");
    let database_path = tree.path.join("catalog.db");
    let (application, collection_ids) = external_search_test_application(
        &tree,
        &["[Circle] Repeated Failure.zip"],
        CatalogRepository::open(&database_path).expect("open persistent catalog"),
    );
    let collection_id = collection_ids[0];
    let (application, first_job) = fail_external_search_job(
        application,
        collection_id,
        &[MetadataField::Authors],
        ExternalSearchErrorKind::Unsupported,
        None,
    );
    let server = RunningServer::start(application).await;
    let acknowledged = server
        .request(
            "POST",
            &format!("/api/external-search-jobs/{}/acknowledge", first_job.id),
            &[],
        )
        .await;
    assert_eq!(200, acknowledged.status);
    server.stop().await;

    let application = ApplicationService::new(
        CatalogRepository::open(&database_path).expect("reopen catalog for new job"),
        NoopRecycleBin,
    );
    let (application, second_job) = fail_external_search_job(
        application,
        collection_id,
        &[MetadataField::Authors],
        ExternalSearchErrorKind::Unsupported,
        None,
    );
    assert_ne!(first_job.id, second_job.id);
    let restarted_server = RunningServer::start(application).await;
    let activity = restarted_server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(1, activity.json["actionable_count"]);
    assert_eq!(
        "acknowledged",
        external_search_activity_item(&activity.json, first_job.id)["resolution"]
    );
    let new_item = external_search_activity_item(&activity.json, second_job.id);
    assert_eq!(true, new_item["actionable"]);
    assert_eq!(Value::Null, new_item["resolution"]);
    restarted_server.stop().await;
}

#[tokio::test]
async fn external_search_activity_lists_all_database_failures_and_excludes_inactive_collections() {
    let tree = TestTree::new("external-activity-database-list");
    let filenames = [
        "[Circle] First Actionable.zip",
        "[Circle] Second Actionable.zip",
        "[Circle] Tombstoned.zip",
    ];
    let (application, collection_ids) = external_search_test_application(
        &tree,
        &filenames,
        CatalogRepository::open_in_memory().expect("open catalog"),
    );
    let (application, first_job) = fail_external_search_job(
        application,
        collection_ids[0],
        &[MetadataField::Authors],
        ExternalSearchErrorKind::NoMatch,
        None,
    );
    let (application, second_job) = fail_external_search_job(
        application,
        collection_ids[1],
        &[MetadataField::Authors],
        ExternalSearchErrorKind::NoMatch,
        None,
    );
    let (application, ghost_job) = fail_external_search_job(
        application,
        collection_ids[2],
        &[MetadataField::Authors],
        ExternalSearchErrorKind::NoMatch,
        None,
    );
    fs::remove_file(tree.library().join(filenames[2])).expect("remove tombstoned collection");
    let mut repository = application.into_repository();
    repository
        .mark_collection_missing(collection_ids[2])
        .expect("tombstone inactive collection");
    let server = RunningServer::start(ApplicationService::new(repository, NoopRecycleBin)).await;

    let activity = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, activity.status);
    assert_eq!(2, activity.json["actionable_count"]);
    assert_eq!(
        true,
        external_search_activity_item(&activity.json, first_job.id)["actionable"]
    );
    assert_eq!(
        true,
        external_search_activity_item(&activity.json, second_job.id)["actionable"]
    );
    assert!(
        activity.json["items"]
            .as_array()
            .expect("activity items")
            .iter()
            .all(|item| item["id"].as_i64() != Some(ghost_job.id)),
        "inactive collection job must not remain as an Activity ghost"
    );
    server.stop().await;
}

#[tokio::test]
async fn external_search_activity_does_not_count_pending_or_running_jobs_as_actionable() {
    let tree = TestTree::new("external-activity-nonterminal");
    let (mut application, collection_ids) = external_search_test_application(
        &tree,
        &["[Circle] Pending.zip", "[Circle] Running.zip"],
        CatalogRepository::open_in_memory().expect("open catalog"),
    );
    let pending_job = application
        .enqueue_external_search(collection_ids[0], &[MetadataField::Authors])
        .expect("enqueue pending job")
        .job;
    let running_job = application
        .enqueue_external_search(collection_ids[1], &[MetadataField::Authors])
        .expect("enqueue running job")
        .job;
    let mut repository = application.into_repository();
    repository
        .start_external_search_job(running_job.id)
        .expect("start running job");
    let server = RunningServer::start(ApplicationService::new(repository, NoopRecycleBin)).await;

    let activity = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, activity.status);
    assert_eq!(0, activity.json["actionable_count"]);
    for job_id in [pending_job.id, running_job.id] {
        let item = external_search_activity_item(&activity.json, job_id);
        assert_eq!(false, item["actionable"]);
        assert_eq!(Value::Null, item["resolution"]);
        assert_eq!(Value::Null, item["acknowledged_at"]);
    }
    server.stop().await;
}

#[tokio::test]
async fn activity_ui_reads_persistent_external_search_activity_and_offers_acknowledgement() {
    let application = ApplicationService::new(
        CatalogRepository::open_in_memory().expect("open catalog"),
        NoopRecycleBin,
    );
    let server = RunningServer::start(application).await;
    let javascript = server.request("GET", "/assets/app.js", &[]).await;
    assert_eq!(200, javascript.status);
    let script = String::from_utf8(javascript.body).expect("UTF-8 script");

    assert!(script.contains("/api/external-search-jobs/activity"));
    assert!(script.contains("/acknowledge"));
    assert!(script.contains("標記已處理"));
    assert!(script.contains("metadata_resolved"));
    assert!(script.contains("actionable_count"));
    server.stop().await;
}

#[tokio::test]
async fn external_search_batches_preflight_and_persist_100_plus_only_missing_jobs() {
    let tree = TestTree::new("external-search-batches");
    for index in 0..101 {
        tree.zip(&format!("[Circle] batch {index:03}.zip"));
    }
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Archive, "歸檔區")
        .expect("register root");
    let roots = repository.active_scan_roots().expect("active roots");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application.run_scan(&roots).expect("scan collections");
    let mut collection_ids = Vec::new();
    for index in 0..101 {
        collection_ids.push(
            application
                .repository()
                .collection_id_for_current_path(
                    &tree
                        .library()
                        .join(format!("[Circle] batch {index:03}.zip")),
                )
                .expect("collection lookup")
                .expect("collection ID"),
        );
    }
    application
        .set_manual_metadata(
            collection_ids[1],
            MetadataField::Event,
            MetadataValue::Text("C106".to_owned()),
        )
        .expect("seed existing event");
    let existing = application
        .enqueue_external_search(collection_ids[0], &[MetadataField::Parody])
        .expect("seed active job")
        .job;
    let server = RunningServer::start(application).await;
    let request_ids = collection_ids
        .iter()
        .copied()
        .chain(std::iter::once(collection_ids[0]))
        .collect::<Vec<_>>();
    let body = serde_json::json!({
        "collection_ids": request_ids,
        "fields": ["event", "parody", "event"],
        "strategy": "only_missing"
    });

    let preflight = server
        .request_json("POST", "/api/external-search-batches/preflight", &body)
        .await;
    assert_eq!(200, preflight.status);
    assert_eq!(101, preflight.json["total"]);
    assert_eq!(100, preflight.json["will_enqueue"]);
    assert_eq!(1, preflight.json["reused"]);
    assert_eq!(0, preflight.json["skipped"]);
    assert_eq!(0, preflight.json["unchanged"]);
    assert_eq!(100, preflight.json["field_needs"][0]["count"]);
    assert_eq!(101, preflight.json["field_needs"][1]["count"]);
    let second = preflight.json["items"]
        .as_array()
        .expect("preflight items")
        .iter()
        .find(|item| item["collection_id"] == collection_ids[1])
        .expect("second item");
    assert_eq!(serde_json::json!(["parody"]), second["fields"]);

    let created = server
        .request_json("POST", "/api/external-search-batches", &body)
        .await;
    assert_eq!(200, created.status);
    assert_eq!(101, created.json["summary"]["total"]);
    assert_eq!(101, created.json["summary"]["pending"]);
    assert_eq!(1, created.json["summary"]["reused"]);
    let batch_id = created.json["id"].as_i64().expect("batch ID");
    let reused = created.json["items"]
        .as_array()
        .expect("batch items")
        .iter()
        .find(|item| item["outcome"] == "reused")
        .expect("reused item");
    assert_eq!(existing.id, reused["job_id"]);
    assert_eq!("pending", reused["status"]);

    let fetched = server
        .request(
            "GET",
            &format!("/api/external-search-batches/{batch_id}"),
            &[],
        )
        .await;
    assert_eq!(200, fetched.status);
    assert_eq!(101, fetched.json["items"].as_array().expect("items").len());
    let retry = server
        .request(
            "POST",
            &format!("/api/external-search-batches/{batch_id}/retry"),
            &[],
        )
        .await;
    assert_eq!(400, retry.status);
    assert_eq!("invalid_external_search_batch", retry.json["error"]["code"]);

    for invalid in [
        serde_json::json!({"collection_ids": collection_ids, "fields": ["path"], "strategy": "only_missing"}),
        serde_json::json!({"collection_ids": [1], "fields": ["title"], "strategy": "overwrite"}),
    ] {
        assert_eq!(
            400,
            server
                .request_json("POST", "/api/external-search-batches/preflight", &invalid)
                .await
                .status
        );
    }
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

fn fake_ehentai_services(
    cookies: CookieStore,
    error: Option<SourceErrorKind>,
    launcher: RecordingMagnetLauncher,
    environment_override: bool,
) -> EhentaiHttpServices {
    EhentaiHttpServices::new(
        cookies.clone(),
        Arc::new(FakeEhentaiSource { cookies, error }),
        Arc::new(launcher),
        environment_override,
    )
}

#[tokio::test]
async fn ehentai_api_updates_shared_session_and_maps_source_dtos_without_cookie_leakage() {
    let store = CookieStore::default();
    let launcher = RecordingMagnetLauncher::default();
    let services = fake_ehentai_services(store.clone(), None, launcher.clone(), false);
    let application = ApplicationService::new(
        CatalogRepository::open_in_memory().expect("catalog"),
        NoopRecycleBin,
    );
    let server = RunningServer::start_with_ehentai(application, services).await;

    let initial = server.request("GET", "/api/ehentai/session", &[]).await;
    assert_eq!(200, initial.status);
    assert_eq!(false, initial.json["configured"]);
    assert_eq!(false, initial.json["environment_override"]);
    assert_eq!(Value::Null, initial.json["session"]);

    let secret = "ipb_member_id=1; ipb_pass_hash=do-not-leak; igneous=fixture";
    let saved = server
        .request_json(
            "PUT",
            "/api/ehentai/session",
            &serde_json::json!({"cookie": secret}),
        )
        .await;
    assert_eq!(200, saved.status);
    assert_eq!(true, saved.json["configured"]);
    assert!(!String::from_utf8_lossy(&saved.body).contains("do-not-leak"));
    assert!(store.is_configured());

    let tested = server
        .request("POST", "/api/ehentai/session/test", &[])
        .await;
    assert_eq!(200, tested.status);
    assert_eq!("exhentai", tested.json["session"]);

    let search = server
        .request(
            "GET",
            "/api/ehentai/search?q=fixture&page=2&cursor=4126558",
            &[],
        )
        .await;
    assert_eq!(200, search.status);
    assert_eq!("ehentai", search.json["source"]);
    assert_eq!(2, search.json["page"]);
    assert_eq!(true, search.json["has_next"]);
    assert_eq!("4126558", search.json["next_cursor"]);
    assert_eq!("prev:4127000", search.json["previous_cursor"]);
    assert_eq!("1700000000", search.json["items"][0]["posted_at"]);
    assert_eq!(42, search.json["items"][0]["pages"]);

    let invalid_cursor = server
        .request(
            "GET",
            "/api/ehentai/search?q=fixture&page=2&cursor=not-digits",
            &[],
        )
        .await;
    assert_eq!(400, invalid_cursor.status);
    assert_eq!("invalid_search_query", invalid_cursor.json["error"]["code"]);

    let previous_cursor = server
        .request(
            "GET",
            "/api/ehentai/search?q=fixture&page=1&cursor=prev%3A4127000",
            &[],
        )
        .await;
    assert_eq!(200, previous_cursor.status);

    let detail = server
        .request("GET", "/api/ehentai/galleries/123/0123456789", &[])
        .await;
    assert_eq!(200, detail.status);
    assert_eq!("ehentai", detail.json["source"]);
    assert_eq!("日本語", detail.json["gallery"]["title_jpn"]);

    let torrents = server
        .request("GET", "/api/ehentai/galleries/123/0123456789/torrents", &[])
        .await;
    assert_eq!(200, torrents.status);
    assert_eq!(9, torrents.json["items"][0]["seeds"]);
    assert_eq!(7, torrents.json["items"][0]["downloads"]);

    let download = server
        .request_json(
            "POST",
            "/api/ehentai/torrents/download",
            &serde_json::json!({
                "url": "https://ehtracker.org/get/fixture?secret=hidden",
                "name": "bad:\r\n名稱/part"
            }),
        )
        .await;
    assert_eq!(200, download.status);
    assert_eq!(
        Some("application/x-bittorrent"),
        download.header("content-type")
    );
    let disposition = download
        .header("content-disposition")
        .expect("attachment header");
    assert!(disposition.starts_with("attachment; filename=\""));
    assert!(disposition.ends_with(".torrent\""));
    assert!(!disposition.contains(['\r', '\n']));
    assert!(download.body.starts_with(b"d4:info"));

    let magnet = format!("magnet:?xt=urn:btih:{}", "a".repeat(40));
    let opened = server
        .request_json(
            "POST",
            "/api/ehentai/magnets/open",
            &serde_json::json!({"magnet_uri": magnet}),
        )
        .await;
    assert_eq!(200, opened.status);
    assert_eq!(true, opened.json["opened"]);
    assert_eq!(1, launcher.calls().len());

    let invalid_magnet = server
        .request_json(
            "POST",
            "/api/ehentai/magnets/open",
            &serde_json::json!({"magnet_uri": "magnet:?xt=urn:btih:short%0d%0a"}),
        )
        .await;
    assert_eq!(400, invalid_magnet.status);
    assert_eq!("invalid_magnet", invalid_magnet.json["error"]["code"]);
    assert_eq!(1, launcher.calls().len());

    let cleared = server.request("DELETE", "/api/ehentai/session", &[]).await;
    assert_eq!(200, cleared.status);
    assert_eq!(false, cleared.json["configured"]);
    assert_eq!("not_configured", cleared.json["session"]);
    assert!(!store.is_configured());
    server.stop().await;
}

#[tokio::test]
async fn ehentai_environment_override_keeps_effective_cookie_while_catalog_backup_changes() {
    let environment =
        CookieHeader::parse("ipb_member_id=1; ipb_pass_hash=environment-secret; igneous=fixture")
            .expect("environment cookie");
    let store = CookieStore::new(Some(environment));
    let services = fake_ehentai_services(
        store.clone(),
        None,
        RecordingMagnetLauncher::default(),
        true,
    );
    let application = ApplicationService::new(
        CatalogRepository::open_in_memory().expect("catalog"),
        NoopRecycleBin,
    );
    let server = RunningServer::start_with_ehentai(application, services).await;

    let saved = server
        .request_json(
            "PUT",
            "/api/ehentai/session",
            &serde_json::json!({
                "cookie": "ipb_member_id=2; ipb_pass_hash=catalog-secret"
            }),
        )
        .await;
    assert_eq!(200, saved.status);
    assert_eq!(true, saved.json["configured"]);
    assert_eq!(true, saved.json["environment_override"]);
    assert!(
        store
            .snapshot()
            .expect("effective cookie")
            .request_header_value()
            .to_str()
            .expect("header")
            .contains("environment-secret")
    );

    let cleared = server.request("DELETE", "/api/ehentai/session", &[]).await;
    assert_eq!(200, cleared.status);
    assert_eq!(true, cleared.json["configured"]);
    assert_eq!(true, cleared.json["environment_override"]);
    assert!(store.is_configured());
    server.stop().await;
}

#[tokio::test]
async fn ehentai_source_errors_use_stable_codes_and_redact_provider_messages() {
    let services = fake_ehentai_services(
        CookieStore::default(),
        Some(SourceErrorKind::NetworkError),
        RecordingMagnetLauncher::default(),
        false,
    );
    let application = ApplicationService::new(
        CatalogRepository::open_in_memory().expect("catalog"),
        NoopRecycleBin,
    );
    let server = RunningServer::start_with_ehentai(application, services).await;
    let response = server
        .request("GET", "/api/ehentai/search?q=fixture", &[])
        .await;
    assert_eq!(502, response.status);
    assert_eq!("network_error", response.json["error"]["code"]);
    let body = String::from_utf8_lossy(&response.body);
    assert!(!body.contains("secret=hidden"));
    assert!(!body.contains("example.invalid"));
    server.stop().await;
}

#[tokio::test]
async fn ehentai_unavailable_catalog_cookie_can_be_cleared_and_replaced() {
    let tree = TestTree::new("ehentai-damaged-cookie");
    let database = tree.path.join("catalog.db");
    let mut repository = CatalogRepository::open(&database).expect("catalog");
    let secret = "ipb_member_id=1; ipb_pass_hash=do-not-leak";
    repository
        .save_exhentai_cookie(secret)
        .expect("save protected cookie");
    drop(repository);
    rusqlite::Connection::open(&database)
        .expect("raw catalog")
        .execute(
            "UPDATE exhentai_session SET encrypted_cookie = X'010203' WHERE singleton = 1",
            [],
        )
        .expect("damage protected cookie");
    let application = ApplicationService::new(
        CatalogRepository::open(&database).expect("reopen catalog"),
        NoopRecycleBin,
    );
    let services = fake_ehentai_services(
        CookieStore::default(),
        None,
        RecordingMagnetLauncher::default(),
        false,
    );
    let server = RunningServer::start_with_ehentai(application, services).await;

    let unavailable = server.request("GET", "/api/ehentai/session", &[]).await;
    assert_eq!(409, unavailable.status);
    assert_eq!("invalid_cookie", unavailable.json["error"]["code"]);
    assert!(!String::from_utf8_lossy(&unavailable.body).contains("do-not-leak"));

    let cleared = server.request("DELETE", "/api/ehentai/session", &[]).await;
    assert_eq!(200, cleared.status);
    let replacement = server
        .request_json(
            "PUT",
            "/api/ehentai/session",
            &serde_json::json!({
                "cookie": "ipb_member_id=2; ipb_pass_hash=replacement"
            }),
        )
        .await;
    assert_eq!(200, replacement.status);
    assert_eq!(true, replacement.json["configured"]);
    server.stop().await;

    rusqlite::Connection::open(&database)
        .expect("raw catalog after replacement")
        .execute(
            "UPDATE exhentai_session SET encrypted_cookie = X'010203' WHERE singleton = 1",
            [],
        )
        .expect("damage replacement cookie");
    let environment_store = CookieStore::new(Some(
        CookieHeader::parse("ipb_member_id=9; ipb_pass_hash=environment")
            .expect("environment cookie"),
    ));
    let services = fake_ehentai_services(
        environment_store,
        None,
        RecordingMagnetLauncher::default(),
        true,
    );
    let application = ApplicationService::new(
        CatalogRepository::open(&database).expect("reopen damaged catalog"),
        NoopRecycleBin,
    );
    let server = RunningServer::start_with_ehentai(application, services).await;
    let overridden = server.request("GET", "/api/ehentai/session", &[]).await;
    assert_eq!(200, overridden.status);
    assert_eq!(true, overridden.json["configured"]);
    assert_eq!(true, overridden.json["environment_override"]);
    server.stop().await;
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

fn cover_application(tree: &TestTree) -> (ApplicationService<NoopRecycleBin>, i64) {
    let entries = (0..26)
        .map(|index| {
            (
                format!("page{index:02}.png"),
                [u8::try_from(index).expect("color"), 20, 40, 255],
            )
        })
        .collect::<Vec<_>>();
    let references = entries
        .iter()
        .map(|(name, color)| (name.as_str(), *color))
        .collect::<Vec<_>>();
    tree.image_zip("[circle] cover candidates.zip", &references);
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
        .expect("scan cover collection");
    let collection_id = application
        .repository()
        .first_collection_id()
        .expect("collection query")
        .expect("collection ID");
    (application, collection_id)
}

#[tokio::test]
async fn cover_selection_api_lists_bounded_candidates_previews_and_persists_invalid_state() {
    let tree = TestTree::new("cover-selection-api");
    let (application, collection_id) = cover_application(&tree);
    let server = RunningServer::start(application).await;

    let bounded = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/cover-candidates?limit=100"),
            &[],
        )
        .await;
    assert_eq!(200, bounded.status);
    assert_eq!(24, bounded.json["items"].as_array().expect("items").len());
    assert_eq!(Value::Null, bounded.json["selection"]);
    let source_fingerprint = bounded.json["source_fingerprint"]
        .as_str()
        .expect("source fingerprint")
        .to_owned();
    let single = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/cover-candidates?limit=0"),
            &[],
        )
        .await;
    assert_eq!(
        1,
        single.json["items"].as_array().expect("single item").len()
    );

    let preview = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/cover-candidates/preview?entry=page02.png"),
            &[],
        )
        .await;
    assert_eq!(200, preview.status);
    assert_eq!(Some("image/webp"), preview.header("content-type"));
    image::load_from_memory(&preview.body).expect("decode bounded WebP preview");
    let traversal = server
        .request(
            "GET",
            &format!(
                "/api/collections/{collection_id}/cover-candidates/preview?entry=..%2Fescape.png"
            ),
            &[],
        )
        .await;
    assert_eq!(400, traversal.status);
    assert_eq!("invalid_cover_candidate", traversal.json["error"]["code"]);
    let missing_entry = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/cover-candidates/preview"),
            &[],
        )
        .await;
    assert_eq!(400, missing_entry.status);
    assert_eq!("invalid_cover_entry", missing_entry.json["error"]["code"]);

    let extra_field = server
        .request_json(
            "PUT",
            &format!("/api/collections/{collection_id}/cover-selection"),
            &serde_json::json!({
                "entry_path": "page02.png",
                "source_fingerprint": source_fingerprint,
                "index": 2
            }),
        )
        .await;
    assert_eq!(
        422, extra_field.status,
        "selection body must be allowlisted"
    );
    let stale_source = server
        .request_json(
            "PUT",
            &format!("/api/collections/{collection_id}/cover-selection"),
            &serde_json::json!({
                "entry_path": "page02.png",
                "source_fingerprint": "stale"
            }),
        )
        .await;
    assert_eq!(409, stale_source.status);
    assert_eq!("cover_source_changed", stale_source.json["error"]["code"]);
    let selected = server
        .request_json(
            "PUT",
            &format!("/api/collections/{collection_id}/cover-selection"),
            &serde_json::json!({
                "entry_path": "page02.png",
                "source_fingerprint": bounded.json["source_fingerprint"]
            }),
        )
        .await;
    assert_eq!(200, selected.status);
    assert_eq!("page02.png", selected.json["entry_path"]);
    assert_eq!("valid", selected.json["status"]);

    tree.image_zip(
        "[circle] cover candidates.zip",
        &[("page00.png", [0, 20, 40, 255])],
    );
    let invalid = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/cover-candidates"),
            &[],
        )
        .await;
    assert_eq!(200, invalid.status);
    assert_eq!("missing", invalid.json["selection"]["status"]);
    assert_eq!("page02.png", invalid.json["selection"]["entry_path"]);

    let cleared = server
        .request(
            "DELETE",
            &format!("/api/collections/{collection_id}/cover-selection"),
            &[],
        )
        .await;
    assert_eq!(204, cleared.status);
    let automatic = server
        .request(
            "GET",
            &format!("/api/collections/{collection_id}/cover-candidates"),
            &[],
        )
        .await;
    assert_eq!(Value::Null, automatic.json["selection"]);
    server.stop().await;
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
    fs::write(&reader_path, b"reader placeholder").expect("reader executable");
    let server = RunningServer::start(application).await;

    let initial = server.request("GET", "/api/settings", &[]).await;
    assert_eq!(200, initial.status);
    assert_eq!("300x400", initial.json["thumb_size"]);
    assert_eq!(80, initial.json["thumb_quality"]);
    assert_eq!(48, initial.json["library_batch_size"]);
    assert_eq!(serde_json::json!([]), initial.json["environment_overrides"]);

    let updated = server
        .request_json(
            "PUT",
            "/api/settings",
            &serde_json::json!({
                "viewer_path": reader_path.to_string_lossy(),
                "thumb_size": "360x480",
                "thumb_quality": 85,
                "library_batch_size": 96
            }),
        )
        .await;
    assert_eq!(200, updated.status);
    assert_eq!("360x480", updated.json["thumb_size"]);
    assert_eq!(85, updated.json["thumb_quality"]);
    assert_eq!(96, updated.json["library_batch_size"]);
    assert_eq!(1, updated.json["thumbnails_requeued"]);

    for library_batch_size in [24, 48, 96, 144, 192] {
        let round_trip = server
            .request_json(
                "PUT",
                "/api/settings",
                &serde_json::json!({
                    "viewer_path": reader_path.to_string_lossy(),
                    "thumb_size": "360x480",
                    "thumb_quality": 85,
                    "library_batch_size": library_batch_size
                }),
            )
            .await;
        assert_eq!(200, round_trip.status);
        assert_eq!(library_batch_size, round_trip.json["library_batch_size"]);
        assert_eq!(0, round_trip.json["thumbnails_requeued"]);
    }

    for payload in [
        serde_json::json!({
            "viewer_path": "",
            "thumb_size": "300*400",
            "thumb_quality": 80,
            "library_batch_size": 48
        }),
        serde_json::json!({
            "viewer_path": "",
            "thumb_size": "300x400",
            "thumb_quality": 0,
            "library_batch_size": 48
        }),
        serde_json::json!({
            "viewer_path": "",
            "thumb_size": "300x400",
            "thumb_quality": 80,
            "library_batch_size": 48,
            "unknown": true
        }),
        serde_json::json!({
            "viewer_path": tree.root("missing-reader.exe").to_string_lossy(),
            "thumb_size": "300x400",
            "thumb_quality": 80,
            "library_batch_size": 48
        }),
    ] {
        let invalid = server.request_json("PUT", "/api/settings", &payload).await;
        assert_eq!(400, invalid.status);
    }
    let invalid_batch_size = server
        .request_json(
            "PUT",
            "/api/settings",
            &serde_json::json!({
                "viewer_path": reader_path.to_string_lossy(),
                "thumb_size": "360x480",
                "thumb_quality": 85,
                "library_batch_size": 25
            }),
        )
        .await;
    assert_eq!(400, invalid_batch_size.status);
    assert_eq!(
        "invalid_library_batch_size",
        invalid_batch_size.json["error"]["code"]
    );
    let retained = server.request("GET", "/api/settings", &[]).await;
    assert_eq!("360x480", retained.json["thumb_size"]);
    assert_eq!(85, retained.json["thumb_quality"]);
    assert_eq!(192, retained.json["library_batch_size"]);
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
        .save_application_settings(Some(saved_reader.clone()), 360, 480, 85, None, 144)
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
    assert_eq!(144, response.json["library_batch_size"]);
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
async fn settings_api_manages_default_archive_root_and_pins_stored_value_after_deactivation() {
    let tree = TestTree::new("settings-default-archive-root");
    fs::create_dir_all(tree.library()).expect("create archive library");
    let downloads_path = tree.path.join("downloads-library");
    fs::create_dir_all(&downloads_path).expect("create downloads library");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let thumbnail_config =
        ThumbnailConfig::new(tree.path.join("cache"), 300, 400, 80).expect("thumbnail config");
    let application = ApplicationService::with_thumbnails(repository, NoopRecycleBin, thumbnail_config);
    let server = RunningServer::start(application).await;

    let archive_root = server
        .request_json(
            "POST",
            "/api/library-roots",
            &serde_json::json!({
                "path": tree.library(),
                "source": "archive",
                "label": "典藏區"
            }),
        )
        .await;
    assert_eq!(200, archive_root.status);
    let archive_root_id = archive_root.json["id"].as_i64().expect("archive root ID");

    let downloads_root = server
        .request_json(
            "POST",
            "/api/library-roots",
            &serde_json::json!({
                "path": downloads_path,
                "source": "downloads",
                "label": "下載區"
            }),
        )
        .await;
    assert_eq!(200, downloads_root.status);
    let downloads_root_id = downloads_root.json["id"].as_i64().expect("downloads root ID");

    let settings_payload = |default_archive_root_id: Option<i64>| {
        serde_json::json!({
            "viewer_path": "",
            "thumb_size": "300x400",
            "thumb_quality": 80,
            "default_archive_root_id": default_archive_root_id,
            "library_batch_size": 48
        })
    };

    let initial = server.request("GET", "/api/settings", &[]).await;
    assert_eq!(200, initial.status);
    assert_eq!(Value::Null, initial.json["default_archive_root_id"]);

    let set = server
        .request_json(
            "PUT",
            "/api/settings",
            &settings_payload(Some(archive_root_id)),
        )
        .await;
    assert_eq!(200, set.status);
    assert_eq!(archive_root_id, set.json["default_archive_root_id"]);

    let confirmed = server.request("GET", "/api/settings", &[]).await;
    assert_eq!(200, confirmed.status);
    assert_eq!(archive_root_id, confirmed.json["default_archive_root_id"]);

    let downloads_rejected = server
        .request_json(
            "PUT",
            "/api/settings",
            &settings_payload(Some(downloads_root_id)),
        )
        .await;
    assert_eq!(400, downloads_rejected.status);
    assert_eq!(
        "invalid_settings",
        downloads_rejected.json["error"]["code"]
    );

    let missing_rejected = server
        .request_json("PUT", "/api/settings", &settings_payload(Some(999)))
        .await;
    assert_eq!(400, missing_rejected.status);
    assert_eq!("invalid_settings", missing_rejected.json["error"]["code"]);

    let unchanged = server.request("GET", "/api/settings", &[]).await;
    assert_eq!(200, unchanged.status);
    assert_eq!(archive_root_id, unchanged.json["default_archive_root_id"]);

    let cleared = server
        .request_json("PUT", "/api/settings", &settings_payload(None))
        .await;
    assert_eq!(200, cleared.status);
    assert_eq!(Value::Null, cleared.json["default_archive_root_id"]);

    let cleared_confirmed = server.request("GET", "/api/settings", &[]).await;
    assert_eq!(200, cleared_confirmed.status);
    assert_eq!(Value::Null, cleared_confirmed.json["default_archive_root_id"]);

    let reset = server
        .request_json(
            "PUT",
            "/api/settings",
            &settings_payload(Some(archive_root_id)),
        )
        .await;
    assert_eq!(200, reset.status);
    assert_eq!(archive_root_id, reset.json["default_archive_root_id"]);

    let deactivated = server
        .request(
            "DELETE",
            &format!("/api/library-roots/{archive_root_id}"),
            &[],
        )
        .await;
    assert_eq!(200, deactivated.status);
    assert_eq!(false, deactivated.json["active"]);

    let after_deactivation = server.request("GET", "/api/settings", &[]).await;
    assert_eq!(200, after_deactivation.status);
    assert_eq!(
        archive_root_id,
        after_deactivation.json["default_archive_root_id"]
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
async fn scan_preflight_and_no_rename_share_a_read_only_contract() {
    let tree = TestTree::new("scan-preflight");
    let original_name = "%28C77%29%20%5Bcircle%5D%20title.zip";
    tree.zip(original_name);
    let original = tree.library().join(original_name);
    let renamed = tree.library().join("(C77) [circle] title.zip");
    let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
    repository
        .register_library_root(&tree.library(), SourceKind::Downloads, "下載區")
        .expect("register root");
    let application = ApplicationService::new(repository, NoopRecycleBin);
    let server = RunningServer::start(application).await;

    let preflight = server.request("POST", "/api/scans/preflight", &[]).await;
    assert_eq!(200, preflight.status);
    assert_eq!(1, preflight.json["expectation"]["new_collections"]);
    assert_eq!(0, preflight.json["expectation"]["already_known"]);
    assert_eq!(1, preflight.json["expectation"]["planned_renames"]);
    assert_eq!(
        Some(original.to_string_lossy().as_ref()),
        preflight.json["renames"][0]["before"].as_str()
    );
    assert_eq!(
        Some(renamed.to_string_lossy().as_ref()),
        preflight.json["renames"][0]["after"].as_str()
    );
    assert!(original.exists());
    assert!(!renamed.exists());
    let latest = server.request("GET", "/api/scans/latest", &[]).await;
    assert!(latest.json["scan"].is_null());

    let scan = server
        .request_json(
            "POST",
            "/api/scans",
            &serde_json::json!({
                "mode": "no_rename",
                "expected": preflight.json["expectation"].clone(),
            }),
        )
        .await;
    assert_eq!(200, scan.status);
    assert_eq!(1, scan.json["summary"]["added"]);
    assert_eq!(0, scan.json["summary"]["renamed"]);
    assert_eq!(1, scan.json["summary"]["planned_renames"]);
    assert_eq!(
        serde_json::json!([]),
        scan.json["summary"]["preflight_differences"]
    );
    assert!(original.exists());
    assert!(!renamed.exists());

    let invalid = server
        .request_json(
            "POST",
            "/api/scans",
            &serde_json::json!({ "mode": "unsafe" }),
        )
        .await;
    assert_eq!(400, invalid.status);
    assert_eq!("invalid_scan_mode", invalid.json["error"]["code"]);
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
    let (application, merged_job) = fail_external_search_job(
        application,
        candidate_id,
        &[MetadataField::Authors],
        ExternalSearchErrorKind::NoMatch,
        None,
    );
    let server = RunningServer::start(application).await;
    let base = format!("/api/tombstone-candidates/{old_id}/{candidate_id}");

    let activity_before = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, activity_before.status);
    assert_eq!(1, activity_before.json["actionable_count"]);
    assert_eq!(
        true,
        external_search_activity_item(&activity_before.json, merged_job.id)["actionable"]
    );

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

    let activity_after = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, activity_after.status);
    assert_eq!(0, activity_after.json["actionable_count"]);
    assert!(
        activity_after.json["items"]
            .as_array()
            .expect("activity items after consolidation")
            .iter()
            .all(|item| item["id"].as_i64() != Some(merged_job.id)),
        "merged collection job must not remain as an Activity ghost"
    );

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
async fn vocabulary_api_preflights_merges_rejects_and_stays_separate_from_identity() {
    let tree = TestTree::new("vocabulary-api");
    for filename in [
        "[event-wide] event-wide.zip",
        "[event-ascii] event-ascii.zip",
        "[circle-dot] circle-dot.zip",
        "[circle-space] circle-space.zip",
    ] {
        tree.zip(filename);
    }
    let root = ScanRoot {
        path: tree.library(),
        source: SourceKind::Archive,
        label: "歸檔區".to_owned(),
    };
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(std::slice::from_ref(&root))
        .expect("scan vocabulary fixtures");
    let collection_id = |filename: &str| {
        application
            .repository()
            .collection_id_for_current_path(&tree.library().join(filename))
            .expect("collection lookup")
            .expect("collection ID")
    };
    let event_wide_id = collection_id("[event-wide] event-wide.zip");
    let event_ascii_id = collection_id("[event-ascii] event-ascii.zip");
    let circle_dot_id = collection_id("[circle-dot] circle-dot.zip");
    let circle_space_id = collection_id("[circle-space] circle-space.zip");
    for (collection_id, field, value) in [
        (event_wide_id, MetadataField::Event, "Ｃ１００"),
        (event_ascii_id, MetadataField::Event, "C100"),
        (circle_dot_id, MetadataField::Circle, "Circle・Name"),
        (circle_space_id, MetadataField::Circle, "circle name"),
    ] {
        application
            .set_manual_metadata(collection_id, field, MetadataValue::Text(value.to_owned()))
            .expect("seed vocabulary value");
    }
    let server = RunningServer::start(application).await;

    let saved = server
        .request_json(
            "POST",
            "/api/saved-views",
            &serde_json::json!({
                "name": "舊場次名稱",
                "pinned": true,
                "query": {
                    "event": "Ｃ１００",
                    "tag": [],
                    "missing": [],
                    "untagged": false,
                    "sort": "created",
                    "direction": "desc",
                    "layout": "grid"
                }
            }),
        )
        .await;
    assert_eq!(201, saved.status);
    let saved_view_id = saved.json["id"].as_i64().expect("saved view ID");

    let candidates = server
        .request("GET", "/api/vocabulary/candidates", &[])
        .await;
    assert_eq!(200, candidates.status);
    let groups = candidates.json["groups"]
        .as_array()
        .expect("candidate groups");
    assert_eq!(2, groups.len());
    let event = groups
        .iter()
        .find(|group| group["field"] == "event")
        .expect("event group");
    assert_eq!(2, event["variants"].as_array().expect("variants").len());
    assert!(
        event["variants"]
            .as_array()
            .expect("variants")
            .iter()
            .all(|variant| variant["active_count"] == 1)
    );

    let invalid_field = server
        .request("GET", "/api/vocabulary/candidates?field=tag", &[])
        .await;
    assert_eq!(400, invalid_field.status);
    assert_eq!(
        "invalid_vocabulary_field",
        invalid_field.json["error"]["code"]
    );
    let unknown_body = server
        .request_json(
            "POST",
            "/api/vocabulary/preflight",
            &serde_json::json!({
                "field": "event",
                "canonical": "C100",
                "variants": ["Ｃ１００", "C100"],
                "tombstone_collection_id": event_wide_id
            }),
        )
        .await;
    assert_eq!(400, unknown_body.status);
    assert_eq!(
        "invalid_vocabulary_request",
        unknown_body.json["error"]["code"]
    );

    let request = serde_json::json!({
        "field": "event",
        "canonical": "C100",
        "variants": ["Ｃ１００", "C100"]
    });
    let preflight = server
        .request_json("POST", "/api/vocabulary/preflight", &request)
        .await;
    assert_eq!(200, preflight.status);
    assert_eq!(2, preflight.json["affected_collections"]);
    assert_eq!(2, preflight.json["manual_assertions"]);
    assert_eq!(1, preflight.json["manual_selected_conflicts"]);
    assert_eq!(saved_view_id, preflight.json["saved_views"][0]["id"]);

    let merged = server
        .request_json("POST", "/api/vocabulary/merge", &request)
        .await;
    assert_eq!(200, merged.status);
    assert_eq!("C100", merged.json["canonical"]);
    assert_eq!(2, merged.json["affected_collections"]);
    assert_eq!(1, merged.json["saved_views_updated"]);
    for collection_id in [event_wide_id, event_ascii_id] {
        let collection = server
            .request("GET", &format!("/api/collections/{collection_id}"), &[])
            .await;
        assert_eq!(200, collection.status);
        assert_eq!("C100", collection.json["event"]);
    }
    let saved_after_merge = server
        .request("GET", &format!("/api/saved-views/{saved_view_id}"), &[])
        .await;
    assert_eq!(200, saved_after_merge.status);
    assert_eq!("C100", saved_after_merge.json["query"]["event"]);
    assert_eq!(2, saved_after_merge.json["result_count"]);

    let rejected = server
        .request_json(
            "POST",
            "/api/vocabulary/reject",
            &serde_json::json!({
                "field": "circle",
                "values": ["Circle・Name", "circle name"],
                "reason": "HTTP test confirms distinct circles"
            }),
        )
        .await;
    assert_eq!(200, rejected.status);
    assert_eq!(1, rejected.json["exclusions_recorded"]);
    let after_reject = server
        .request("GET", "/api/vocabulary/candidates?field=circle", &[])
        .await;
    assert_eq!(200, after_reject.status);
    assert_eq!(
        0,
        after_reject.json["groups"]
            .as_array()
            .expect("groups")
            .len()
    );

    let identity = server
        .request("GET", "/api/tombstone-candidates", &[])
        .await;
    assert_eq!(200, identity.status);
    assert_eq!(
        0,
        identity.json["items"]
            .as_array()
            .expect("identity items")
            .len()
    );
    server.stop().await;
}

#[tokio::test]
async fn vocabulary_suggestions_expose_four_fields_alias_search_limits_and_query_validation() {
    let tree = TestTree::new("vocabulary-suggestions-api");
    for filename in [
        "[suggest-one] one.zip",
        "[suggest-two] two.zip",
        "[suggest-three] three.zip",
    ] {
        tree.zip(filename);
    }
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[ScanRoot {
            path: tree.library(),
            source: SourceKind::Archive,
            label: "歸檔區".to_owned(),
        }])
        .expect("scan suggestion fixtures");
    let filenames = [
        "[suggest-one] one.zip",
        "[suggest-two] two.zip",
        "[suggest-three] three.zip",
    ];
    let mut collection_ids = Vec::new();
    for (index, filename) in filenames.into_iter().enumerate() {
        let collection_id = application
            .repository()
            .collection_id_for_current_path(&tree.library().join(filename))
            .expect("suggestion lookup")
            .expect("suggestion collection");
        let suffix = if index < 2 { "Common" } else { "Rare" };
        for (field, value) in [
            (
                MetadataField::Event,
                MetadataValue::Text(format!("Event {suffix}")),
            ),
            (
                MetadataField::Circle,
                MetadataValue::Text(format!("Circle {suffix}")),
            ),
            (
                MetadataField::Authors,
                MetadataValue::Authors(Authors {
                    raw: Some(format!("Author {suffix}")),
                    values: vec![format!("Author {suffix}")],
                }),
            ),
            (
                MetadataField::Parody,
                MetadataValue::Parody(Parody {
                    raw: format!("Parody {suffix}"),
                    canonical: format!("Parody {suffix}"),
                    evidence: "suggestion HTTP test".to_owned(),
                }),
            ),
        ] {
            application
                .set_manual_metadata(collection_id, field, value)
                .expect("seed suggestion metadata");
        }
        collection_ids.push(collection_id);
    }
    let server = RunningServer::start(application).await;

    for (field, prefix) in [
        ("event", "Event"),
        ("circle", "Circle"),
        ("author", "Author"),
        ("parody", "Parody"),
    ] {
        let top = server
            .request(
                "GET",
                &format!("/api/vocabulary/suggestions?field={field}&limit=1"),
                &[],
            )
            .await;
        assert_eq!(200, top.status, "{field}");
        assert_eq!(1, top.json["items"].as_array().expect("top items").len());
        assert_eq!(format!("{prefix} Common"), top.json["items"][0]["name"]);
        assert_eq!(2, top.json["items"][0]["count"]);

        let searched = server
            .request(
                "GET",
                &format!("/api/vocabulary/suggestions?field={field}&q=rare&limit=20"),
                &[],
            )
            .await;
        assert_eq!(200, searched.status, "{field}");
        assert_eq!(
            1,
            searched.json["items"]
                .as_array()
                .expect("search items")
                .len()
        );
        assert_eq!(format!("{prefix} Rare"), searched.json["items"][0]["name"]);
        assert_eq!(1, searched.json["items"][0]["count"]);
    }

    let merged = server
        .request_json(
            "POST",
            "/api/vocabulary/merge",
            &serde_json::json!({
                "field": "event",
                "canonical": "Event Canonical",
                "variants": ["Event Canonical", "Event Common", "Blue Search Alias"]
            }),
        )
        .await;
    assert_eq!(200, merged.status);
    let alias = server
        .request(
            "GET",
            "/api/vocabulary/suggestions?field=event&q=blue&limit=20",
            &[],
        )
        .await;
    assert_eq!(200, alias.status);
    assert_eq!("Event Canonical", alias.json["items"][0]["name"]);
    assert_eq!(2, alias.json["items"][0]["count"]);
    assert_eq!(
        serde_json::json!(["Blue Search Alias", "Event Common"]),
        alias.json["items"][0]["aliases"]
    );

    let manual = server
        .request_json(
            "PUT",
            &format!("/api/collections/{}/metadata/event", collection_ids[2]),
            &serde_json::json!({"value": "Event Canonical"}),
        )
        .await;
    assert_eq!(200, manual.status);
    assert_eq!("Event Canonical", manual.json["event"]);
    let after_manual = server
        .request(
            "GET",
            "/api/vocabulary/suggestions?field=event&q=canonical&limit=20",
            &[],
        )
        .await;
    assert_eq!(3, after_manual.json["items"][0]["count"]);

    for (path, code) in [
        ("/api/vocabulary/suggestions", "missing_vocabulary_field"),
        (
            "/api/vocabulary/suggestions?field=tag",
            "invalid_vocabulary_field",
        ),
        (
            "/api/vocabulary/suggestions?field=event&unknown=1",
            "invalid_vocabulary_query",
        ),
        (
            "/api/vocabulary/suggestions?field=event&field=circle",
            "invalid_vocabulary_query",
        ),
        (
            "/api/vocabulary/suggestions?field=event&q=a&q=b",
            "invalid_vocabulary_query",
        ),
        (
            "/api/vocabulary/suggestions?field=event&limit=0",
            "invalid_vocabulary_limit",
        ),
        (
            "/api/vocabulary/suggestions?field=event&limit=51",
            "invalid_vocabulary_limit",
        ),
    ] {
        let invalid = server.request("GET", path, &[]).await;
        assert_eq!(400, invalid.status, "{path}");
        assert_eq!(code, invalid.json["error"]["code"], "{path}");
    }
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
async fn move_preflight_reports_readiness_and_destinations_without_touching_files() {
    fn collection_id(application: &ApplicationService<NoopRecycleBin>, path: &Path) -> i64 {
        application
            .repository()
            .collection_id_for_current_path(path)
            .expect("collection lookup")
            .expect("collection")
    }

    let tree = TestTree::new("file-move-preflight");
    tree.zip_in("downloads", "ready.zip");
    tree.zip_in("downloads", "unclassified.zip");
    tree.zip_in("downloads", "collision.zip");
    tree.zip_in("downloads", "gone.zip");
    tree.zip_in("archive", "already-archived.zip");
    let downloads = tree.root("downloads");
    let archive = tree.root("archive");
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[
            ScanRoot {
                path: downloads.clone(),
                source: SourceKind::Downloads,
                label: "下載區".to_owned(),
            },
            ScanRoot {
                path: archive.clone(),
                source: SourceKind::Archive,
                label: "歸檔區".to_owned(),
            },
        ])
        .expect("scan roots");
    let ready_id = collection_id(&application, &downloads.join("ready.zip"));
    let unclassified_id = collection_id(&application, &downloads.join("unclassified.zip"));
    let collision_id = collection_id(&application, &downloads.join("collision.zip"));
    let gone_id = collection_id(&application, &downloads.join("gone.zip"));
    let archived_id = collection_id(&application, &archive.join("already-archived.zip"));
    for (collection, event) in [
        (ready_id, "C106"),
        (collision_id, "C105"),
        (gone_id, "C104"),
    ] {
        application
            .set_manual_metadata(
                collection,
                MetadataField::Event,
                MetadataValue::Text(event.to_owned()),
            )
            .expect("set event");
    }
    assert!(
        application
            .collection(unclassified_id)
            .expect("unclassified snapshot")
            .event
            .is_none()
    );
    let archive_root_id = application
        .library_roots()
        .expect("library roots")
        .into_iter()
        .find(|library_root| library_root.source == SourceKind::Archive)
        .expect("archive root")
        .id;
    let downloads_root_id = application
        .library_roots()
        .expect("library roots")
        .into_iter()
        .find(|library_root| library_root.source == SourceKind::Downloads)
        .expect("downloads root")
        .id;
    fs::remove_file(downloads.join("gone.zip")).expect("remove source before preflight");
    let collision_directory = archive.join("C105");
    fs::create_dir(&collision_directory).expect("create collision directory");
    let collision_target = collision_directory.join("collision.zip");
    fs::write(&collision_target, b"existing archive file").expect("create collision file");
    let missing_collection_id = 999_999;
    let shared = share_application(application);
    let server = RunningServer::start_shared(Arc::clone(&shared)).await;

    let arbitrary_path = server
        .request_json(
            "POST",
            "/api/file-actions/move/preflight",
            &serde_json::json!({
                "collection_ids": [ready_id],
                "archive_root_id": archive_root_id,
                "destination": tree.root("outside")
            }),
        )
        .await;
    assert_eq!(400, arbitrary_path.status);
    assert_eq!("invalid_json", arbitrary_path.json["error"]["code"]);

    let preflight = server
        .request_json(
            "POST",
            "/api/file-actions/move/preflight",
            &serde_json::json!({
                "collection_ids": [
                    ready_id,
                    unclassified_id,
                    collision_id,
                    gone_id,
                    archived_id,
                    missing_collection_id
                ],
                "archive_root_id": archive_root_id
            }),
        )
        .await;
    assert_eq!(200, preflight.status);
    assert_eq!(archive_root_id, preflight.json["archive_root_id"]);
    assert_eq!(6, preflight.json["summary"]["total"]);
    assert_eq!(1, preflight.json["summary"]["ready"]);
    assert_eq!(1, preflight.json["summary"]["ready_unclassified"]);
    assert_eq!(1, preflight.json["summary"]["collision"]);
    assert_eq!(1, preflight.json["summary"]["source_missing"]);
    assert_eq!(1, preflight.json["summary"]["not_downloads"]);
    assert_eq!(1, preflight.json["summary"]["collection_missing"]);
    assert_eq!(0, preflight.json["summary"]["blocked"]);
    let items = preflight.json["items"]
        .as_array()
        .expect("preflight items")
        .clone();
    assert_eq!(6, items.len());
    assert_eq!(ready_id, items[0]["collection_id"]);
    assert_eq!("ready", items[0]["status"]);
    assert_eq!(
        archive
            .join("C106")
            .join("ready.zip")
            .to_string_lossy()
            .into_owned(),
        items[0]["destination"]
    );
    assert!(items[0]["message"].is_null());
    assert_eq!(unclassified_id, items[1]["collection_id"]);
    assert_eq!("ready_unclassified", items[1]["status"]);
    assert_eq!(
        archive
            .join("未分類")
            .join("unclassified.zip")
            .to_string_lossy()
            .into_owned(),
        items[1]["destination"]
    );
    assert_eq!(collision_id, items[2]["collection_id"]);
    assert_eq!("collision", items[2]["status"]);
    assert_eq!(
        collision_target.to_string_lossy().into_owned(),
        items[2]["destination"]
    );
    assert!(
        items[2]["message"]
            .as_str()
            .expect("collision message")
            .contains("collision.zip")
    );
    assert_eq!(gone_id, items[3]["collection_id"]);
    assert_eq!("source_missing", items[3]["status"]);
    assert_eq!(
        archive
            .join("C104")
            .join("gone.zip")
            .to_string_lossy()
            .into_owned(),
        items[3]["destination"]
    );
    assert_eq!(archived_id, items[4]["collection_id"]);
    assert_eq!("not_downloads", items[4]["status"]);
    assert!(items[4]["destination"].is_null());
    assert_eq!(missing_collection_id, items[5]["collection_id"]);
    assert_eq!("collection_missing", items[5]["status"]);

    assert!(!archive.join("C106").exists());
    assert!(!archive.join("C104").exists());
    assert!(!archive.join("未分類").exists());
    assert!(downloads.join("ready.zip").is_file());
    assert!(downloads.join("unclassified.zip").is_file());
    assert!(downloads.join("collision.zip").is_file());
    assert!(archive.join("already-archived.zip").is_file());
    assert_eq!(
        b"existing archive file",
        fs::read(&collision_target)
            .expect("read collision target")
            .as_slice()
    );

    let downloads_target = server
        .request_json(
            "POST",
            "/api/file-actions/move/preflight",
            &serde_json::json!({
                "collection_ids": [ready_id],
                "archive_root_id": downloads_root_id
            }),
        )
        .await;
    assert_eq!(400, downloads_target.status);
    assert_eq!("invalid_settings", downloads_target.json["error"]["code"]);

    let missing_root = server
        .request_json(
            "POST",
            "/api/file-actions/move/preflight",
            &serde_json::json!({
                "collection_ids": [ready_id],
                "archive_root_id": 999_999
            }),
        )
        .await;
    assert_eq!(404, missing_root.status);
    assert_eq!("library_root_not_found", missing_root.json["error"]["code"]);
    server.stop().await;

    let application = shared.lock().expect("application lock");
    assert_eq!(
        0,
        application
            .repository()
            .file_operation_count()
            .expect("file operation count")
    );
}

#[tokio::test]
async fn rename_api_requires_preflight_and_applies_only_expected_safe_paths() {
    let tree = TestTree::new("file-rename-api");
    let first_name = "first.zip";
    let second_name = "second.zip";
    tree.zip_in("downloads", first_name);
    tree.zip_in("downloads", second_name);
    let downloads = tree.root("downloads");
    let first_path = downloads.join(first_name);
    let second_path = downloads.join(second_name);
    let repository = CatalogRepository::open_in_memory().expect("open catalog");
    let mut application = ApplicationService::new(repository, NoopRecycleBin);
    application
        .run_scan(&[ScanRoot {
            path: downloads.clone(),
            source: SourceKind::Downloads,
            label: "下載區".to_owned(),
        }])
        .expect("scan downloads");
    let first_id = application
        .repository()
        .collection_id_for_current_path(&first_path)
        .expect("first lookup")
        .expect("first id");
    let second_id = application
        .repository()
        .collection_id_for_current_path(&second_path)
        .expect("second lookup")
        .expect("second id");
    let server = RunningServer::start(application).await;

    let invalid = server
        .request_json(
            "POST",
            "/api/file-actions/rename/preflight",
            &serde_json::json!({
                "collection_ids": [first_id],
                "template": "{script}"
            }),
        )
        .await;
    assert_eq!(400, invalid.status);
    assert_eq!("invalid_settings", invalid.json["error"]["code"]);

    let unknown_field = server
        .request_json(
            "POST",
            "/api/file-actions/rename/preflight",
            &serde_json::json!({
                "collection_ids": [first_id],
                "template": "{title}",
                "script": "ignored"
            }),
        )
        .await;
    assert_eq!(400, unknown_field.status);
    assert_eq!("invalid_json", unknown_field.json["error"]["code"]);
    let duplicate_ids = server
        .request_json(
            "POST",
            "/api/file-actions/rename/preflight",
            &serde_json::json!({
                "collection_ids": [first_id, first_id],
                "template": "{title}"
            }),
        )
        .await;
    assert_eq!(400, duplicate_ids.status);
    assert_eq!(
        "invalid_collection_ids",
        duplicate_ids.json["error"]["code"]
    );

    let preview = server
        .request_json(
            "POST",
            "/api/file-actions/rename/preflight",
            &serde_json::json!({
                "collection_ids": [first_id, second_id],
                "template": "renamed - {title}"
            }),
        )
        .await;
    assert_eq!(200, preview.status);
    assert_eq!(2, preview.json["summary"]["total"]);
    assert_eq!(2, preview.json["summary"]["safe"]);
    assert_eq!("first.zip", preview.json["items"][0]["before"]);
    assert_eq!("renamed - first.zip", preview.json["items"][0]["after"]);
    let mut expected_items = preview.json["items"]
        .as_array()
        .expect("preview items")
        .iter()
        .map(|item| {
            serde_json::json!({
                "collection_id": item["collection_id"],
                "expected_source": item["expected_source"],
                "expected_destination": item["expected_destination"]
            })
        })
        .collect::<Vec<_>>();
    expected_items[0]["expected_source"] = serde_json::json!(downloads.join("stale.zip"));
    let applied = server
        .request_json(
            "POST",
            "/api/file-actions/rename",
            &serde_json::json!({
                "template": "renamed - {title}",
                "items": expected_items
            }),
        )
        .await;
    assert_eq!(200, applied.status);
    assert_eq!(1, applied.json["succeeded"]);
    assert_eq!(1, applied.json["failed"]);
    assert!(first_path.exists());
    assert!(!second_path.exists());
    assert!(!downloads.join("renamed - first.zip").exists());
    assert!(downloads.join("renamed - second.zip").is_file());

    let retry_preview = server
        .request_json(
            "POST",
            "/api/file-actions/rename/preflight",
            &serde_json::json!({
                "collection_ids": [first_id],
                "template": "renamed - {title}"
            }),
        )
        .await;
    assert_eq!(200, retry_preview.status);
    let retry = serde_json::json!({
        "collection_id": retry_preview.json["items"][0]["collection_id"],
        "expected_source": retry_preview.json["items"][0]["expected_source"],
        "expected_destination": retry_preview.json["items"][0]["expected_destination"]
    });
    let retried = server
        .request_json(
            "POST",
            "/api/file-actions/rename",
            &serde_json::json!({
                "template": "renamed - {title}",
                "items": [retry]
            }),
        )
        .await;
    assert_eq!(200, retried.status);
    assert_eq!(1, retried.json["succeeded"]);
    assert!(!first_path.exists());
    assert!(downloads.join("renamed - first.zip").is_file());
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
    let mut repository = application.into_repository();
    let fail_job = |repository: &mut CatalogRepository, collection_id: i64| {
        let job = repository
            .enqueue_external_search(collection_id, &[MetadataField::Authors])
            .expect("enqueue delete Activity job")
            .job;
        repository
            .start_external_search_job(job.id)
            .expect("start delete Activity job");
        repository
            .fail_external_search_job(
                job.id,
                ExternalSearchErrorKind::NoMatch,
                "delete Activity failure",
                None,
            )
            .expect("fail delete Activity job")
    };
    let soft_job = fail_job(&mut repository, soft_id);
    let permanent_job = fail_job(&mut repository, permanent_id);
    let application = ApplicationService::new(
        repository,
        FakeRecycleBin {
            directory: recycle.clone(),
        },
    );
    let server = RunningServer::start(application).await;

    let activity_before = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, activity_before.status);
    assert_eq!(2, activity_before.json["actionable_count"]);

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
    let activity_after = server
        .request("GET", "/api/external-search-jobs/activity", &[])
        .await;
    assert_eq!(200, activity_after.status);
    assert_eq!(0, activity_after.json["actionable_count"]);
    for job_id in [soft_job.id, permanent_job.id] {
        assert!(
            activity_after.json["items"]
                .as_array()
                .expect("activity items after delete")
                .iter()
                .all(|item| item["id"].as_i64() != Some(job_id)),
            "deleted collection job must not remain as an Activity ghost"
        );
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
