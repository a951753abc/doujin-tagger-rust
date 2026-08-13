use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use doujin_app::external_search::{
    ExternalMetadataProvider, ExternalSearchProviderError, ExternalSearchProviderIssue,
    ExternalSearchProviderResponse, ExternalSearchRequest, ExternalSearchStartOutcome,
};
use doujin_app::{ApplicationResult, ApplicationService, ApplicationSettingsOverrides};
use doujin_files::RecycleBin;
use doujin_http::{
    SharedApplication, bind_loopback, serve_shared_with_shutdown, share_application,
};
use doujin_provider_dlsite::DlsiteExactProvider;
use doujin_provider_ehentai::EhentaiProvider;
use doujin_storage::CatalogRepository;
use doujin_storage::settings::StoredApplicationSettings;
use doujin_storage::thumbnails::DEFAULT_THUMBNAIL_PRIORITY;
use doujin_thumbnails::{ThumbnailConfig, ThumbnailGenerationRequest, generate_thumbnail};
use serde::Deserialize;

const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(1);
const THUMBNAIL_WORKER_COUNT: usize = 2;

struct MetadataProviderChain<P, F> {
    primary: P,
    fallback: F,
}

impl<P, F> ExternalMetadataProvider for MetadataProviderChain<P, F>
where
    P: ExternalMetadataProvider,
    F: ExternalMetadataProvider,
{
    fn search(
        &self,
        request: &ExternalSearchRequest,
    ) -> Result<ExternalSearchProviderResponse, ExternalSearchProviderError> {
        match self.primary.search(request) {
            Ok(mut primary) => {
                let missing_fields = request
                    .fields
                    .iter()
                    .copied()
                    .filter(|field| {
                        !primary
                            .candidates
                            .iter()
                            .any(|candidate| candidate.field == *field)
                    })
                    .collect::<Vec<_>>();
                if missing_fields.is_empty() {
                    return Ok(primary);
                }
                let mut fallback_request = request.clone();
                fallback_request.fields = missing_fields;
                match self.fallback.search(&fallback_request) {
                    Ok(mut fallback) => {
                        primary.candidates.append(&mut fallback.candidates);
                        primary.tags.append(&mut fallback.tags);
                        primary.issues.append(&mut fallback.issues);
                    }
                    Err(error)
                        if matches!(
                            error.kind,
                            doujin_storage::jobs::ExternalSearchErrorKind::NoMatch
                                | doujin_storage::jobs::ExternalSearchErrorKind::Unsupported
                        ) => {}
                    Err(error) => primary.issues.push(provider_issue(error)),
                }
                Ok(primary)
            }
            Err(primary_error) => match self.fallback.search(request) {
                Ok(mut fallback)
                    if !matches!(
                        primary_error.kind,
                        doujin_storage::jobs::ExternalSearchErrorKind::NoMatch
                            | doujin_storage::jobs::ExternalSearchErrorKind::Unsupported
                    ) =>
                {
                    fallback.issues.push(provider_issue(primary_error));
                    Ok(fallback)
                }
                Ok(fallback) => Ok(fallback),
                Err(fallback_error) => Err(combine_provider_errors(primary_error, fallback_error)),
            },
        }
    }
}

fn combine_provider_errors(
    primary: ExternalSearchProviderError,
    fallback: ExternalSearchProviderError,
) -> ExternalSearchProviderError {
    let kind = if provider_error_priority(fallback.kind) > provider_error_priority(primary.kind) {
        fallback.kind
    } else {
        primary.kind
    };
    ExternalSearchProviderError {
        kind,
        message: format!(
            "E-Hentai：{}；DLsite fallback：{}",
            primary.message, fallback.message
        ),
    }
}

fn provider_error_priority(kind: doujin_storage::jobs::ExternalSearchErrorKind) -> u8 {
    if kind.retry_delay_seconds(1).is_some() {
        return 3;
    }
    match kind {
        doujin_storage::jobs::ExternalSearchErrorKind::InvalidResponse => 2,
        doujin_storage::jobs::ExternalSearchErrorKind::NoMatch => 1,
        doujin_storage::jobs::ExternalSearchErrorKind::Unsupported => 0,
        _ => 3,
    }
}

fn provider_issue(error: ExternalSearchProviderError) -> ExternalSearchProviderIssue {
    ExternalSearchProviderIssue {
        field: None,
        kind: error.kind,
        message: error.message,
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("doujin-http 啟動失敗：{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let Some(database) = arguments.next() else {
        usage_and_exit();
    };
    let port = match arguments.next() {
        Some(value) => value
            .to_str()
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or("port 必須是 0 到 65535 的整數")?,
        None => 5000,
    };
    if arguments.next().is_some() {
        usage_and_exit();
    }

    let database = absolute_path(PathBuf::from(database))?;
    let repository = CatalogRepository::open(&database)?;
    let config_path = config_path()?;
    let file_config = load_file_config(&config_path)?;
    let environment = EnvironmentSettings::read()?;
    let resolved = resolve_startup_settings(
        &database,
        config_path.parent().unwrap_or_else(|| Path::new(".")),
        file_config,
        repository.stored_application_settings()?,
        environment,
    )?;
    let mut application = ApplicationService::with_system_services_and_overrides(
        repository,
        resolved.reader_path,
        resolved.thumbnail_config,
        resolved.overrides,
    );
    let file_recovery = application.recover_pending_file_operations()?;
    if !file_recovery.items.is_empty() {
        println!(
            "已檢查 {} 筆 pending file operations：{} 成功、{} 失敗、{} 待人工復原",
            file_recovery.items.len(),
            file_recovery.succeeded(),
            file_recovery.failed(),
            file_recovery.pending_recovery()
        );
    }
    let recovered = application.recover_interrupted_external_search_jobs()?;
    if recovered > 0 {
        println!("已回復 {recovered} 筆中斷的 external search jobs");
    }
    let recovered_thumbnails = application.recover_interrupted_thumbnails()?;
    if recovered_thumbnails > 0 {
        println!("已回復 {recovered_thumbnails} 筆中斷的 thumbnail jobs");
    }
    match application.reconcile_thumbnail_settings() {
        Ok(enqueued) if enqueued > 0 => {
            println!("thumbnail 設定或來源變更，已重新排程 {enqueued} 筆既有工作");
        }
        Ok(_) => {}
        Err(error) => eprintln!("無法完整檢查既有 thumbnail 狀態：{error}"),
    }
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let listener = bind_loopback(address).await?;
    println!("doujin-http listening on http://{}", listener.local_addr()?);

    let application = share_application(application);
    let stopping = Arc::new(AtomicBool::new(false));
    let worker_application = Arc::clone(&application);
    let worker_stopping = Arc::clone(&stopping);
    // reqwest's blocking client owns a small internal runtime. Building it on
    // Tokio's async worker can panic when that runtime is dropped, so create it
    // on the same kind of blocking thread that will ultimately own it.
    let primary = tokio::task::spawn_blocking(EhentaiProvider::production).await??;
    let fallback = tokio::task::spawn_blocking(DlsiteExactProvider::production).await??;
    let provider = MetadataProviderChain { primary, fallback };
    let external_worker = thread::Builder::new()
        .name("external-search-worker".to_owned())
        .spawn(move || run_external_search_worker(worker_application, provider, worker_stopping))?;
    let mut thumbnail_workers = Vec::with_capacity(THUMBNAIL_WORKER_COUNT);
    for worker_index in 0..THUMBNAIL_WORKER_COUNT {
        let thumbnail_application = Arc::clone(&application);
        let thumbnail_stopping = Arc::clone(&stopping);
        thumbnail_workers.push(
            thread::Builder::new()
                .name(format!("thumbnail-worker-{}", worker_index + 1))
                .spawn(move || run_thumbnail_worker(thumbnail_application, thumbnail_stopping))?,
        );
    }

    let shutdown_stopping = Arc::clone(&stopping);
    let serve_result = serve_shared_with_shutdown(listener, application, async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown_stopping.store(true, Ordering::Release);
    })
    .await;
    stopping.store(true, Ordering::Release);
    if external_worker.join().is_err() {
        return Err("external search worker 非預期中止".into());
    }
    for thumbnail_worker in thumbnail_workers {
        if thumbnail_worker.join().is_err() {
            return Err("thumbnail worker 非預期中止".into());
        }
    }
    serve_result?;
    Ok(())
}

fn run_thumbnail_worker<R>(application: SharedApplication<R>, stopping: Arc<AtomicBool>)
where
    R: RecycleBin + Send + 'static,
{
    while !stopping.load(Ordering::Acquire) {
        let job = match application.lock() {
            Ok(mut application) => match claim_next_queued_thumbnail(&mut application) {
                Ok(job) => job,
                Err(error) => {
                    eprintln!("thumbnail worker 無法領取工作：{error}");
                    None
                }
            },
            Err(_) => {
                eprintln!("thumbnail worker 無法取得 application service");
                return;
            }
        };
        let Some((collection_id, request)) = job else {
            thread::sleep(WORKER_POLL_INTERVAL);
            continue;
        };
        let result = generate_thumbnail(&request);
        match application.lock() {
            Ok(mut application) => {
                if let Err(error) = application.finish_thumbnail_generation(collection_id, result) {
                    eprintln!("thumbnail {collection_id} 無法寫回：{error}");
                }
            }
            Err(_) => {
                eprintln!("thumbnail worker 無法取得 application service");
                return;
            }
        }
    }
}

fn claim_next_queued_thumbnail<R>(
    application: &mut ApplicationService<R>,
) -> ApplicationResult<Option<(i64, ThumbnailGenerationRequest)>>
where
    R: RecycleBin,
{
    let Some(due) = application
        .due_thumbnails_with_min_priority(1, DEFAULT_THUMBNAIL_PRIORITY)?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let collection_id = due.collection_id;
    let request = application.start_thumbnail_generation(collection_id)?;
    Ok(Some((collection_id, request)))
}

fn absolute_path(path: PathBuf) -> Result<PathBuf, std::io::Error> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileSettings {
    viewer_path: Option<PathBuf>,
    thumb_dir: Option<PathBuf>,
    thumb_size: Option<String>,
    thumb_quality: Option<u8>,
}

#[derive(Debug, Default)]
struct EnvironmentSettings {
    reader_path: Option<PathBuf>,
    thumbnail_dir: Option<PathBuf>,
    thumbnail_size: Option<String>,
    thumbnail_quality: Option<String>,
}

impl EnvironmentSettings {
    fn read() -> Result<Self, std::env::VarError> {
        Ok(Self {
            reader_path: std::env::var_os("DOUJIN_READER_PATH")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            thumbnail_dir: std::env::var_os("DOUJIN_THUMB_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            thumbnail_size: optional_environment_text("DOUJIN_THUMB_SIZE")?,
            thumbnail_quality: optional_environment_text("DOUJIN_THUMB_QUALITY")?,
        })
    }
}

struct ResolvedStartupSettings {
    reader_path: Option<PathBuf>,
    thumbnail_config: ThumbnailConfig,
    overrides: ApplicationSettingsOverrides,
}

fn config_path() -> Result<PathBuf, std::io::Error> {
    match std::env::var_os("DOUJIN_CONFIG_PATH").filter(|value| !value.is_empty()) {
        Some(path) => absolute_path(PathBuf::from(path)),
        None => absolute_path(PathBuf::from("config.json")),
    }
}

fn load_file_config(path: &Path) -> Result<FileSettings, Box<dyn std::error::Error>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileSettings::default()),
        Err(error) => Err(error.into()),
    }
}

fn resolve_startup_settings(
    database: &Path,
    config_directory: &Path,
    file: FileSettings,
    stored: Option<StoredApplicationSettings>,
    environment: EnvironmentSettings,
) -> Result<ResolvedStartupSettings, Box<dyn std::error::Error>> {
    let default_cache_dir = {
        let filename = database
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("doujin.db");
        database
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{filename}.thumbnails"))
    };
    let file_reader = file
        .viewer_path
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| resolve_config_path(config_directory, path));
    let file_cache_dir = file
        .thumb_dir
        .map(|path| resolve_config_path(config_directory, path));
    let file_size = file
        .thumb_size
        .as_deref()
        .map(parse_thumbnail_size)
        .transpose()?;
    let stored_reader = stored
        .as_ref()
        .and_then(|settings| settings.reader_path.clone());
    let stored_size = stored
        .as_ref()
        .map(|settings| (settings.thumbnail_width, settings.thumbnail_height));
    let stored_quality = stored.as_ref().map(|settings| settings.thumbnail_quality);
    let environment_size = environment
        .thumbnail_size
        .as_deref()
        .map(parse_thumbnail_size)
        .transpose()?;
    let environment_quality = environment
        .thumbnail_quality
        .as_deref()
        .map(|value| {
            value
                .parse::<u8>()
                .map_err(|_| "DOUJIN_THUMB_QUALITY 必須是 1 到 100 的整數")
        })
        .transpose()?;
    let reader_path = environment
        .reader_path
        .clone()
        .or(stored_reader)
        .or(file_reader);
    if reader_path
        .as_deref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err("reader path 必須是絕對路徑".into());
    }
    let cache_dir = environment
        .thumbnail_dir
        .clone()
        .or(file_cache_dir)
        .unwrap_or(default_cache_dir);
    let (width, height) = environment_size
        .or(stored_size)
        .or(file_size)
        .unwrap_or((300, 400));
    let quality = environment_quality
        .or(stored_quality)
        .or(file.thumb_quality)
        .unwrap_or(80);
    let thumbnail_config = ThumbnailConfig::new(cache_dir, width, height, quality)?;
    Ok(ResolvedStartupSettings {
        reader_path,
        thumbnail_config,
        overrides: ApplicationSettingsOverrides {
            reader_path: environment.reader_path,
            thumbnail_size: environment_size,
            thumbnail_quality: environment_quality,
        },
    })
}

fn resolve_config_path(config_directory: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        config_directory.join(path)
    }
}

fn optional_environment_text(name: &str) -> Result<Option<String>, std::env::VarError> {
    match std::env::var(name) {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error),
    }
}

fn parse_thumbnail_size(value: &str) -> Result<(u32, u32), &'static str> {
    let (width, height) = value
        .split_once('x')
        .ok_or("thumbnail size 必須使用 WIDTHxHEIGHT 格式")?;
    let width = width
        .parse::<u32>()
        .map_err(|_| "thumbnail size 寬度無效")?;
    let height = height
        .parse::<u32>()
        .map_err(|_| "thumbnail size 高度無效")?;
    Ok((width, height))
}

fn run_external_search_worker<R, P>(
    application: SharedApplication<R>,
    provider: P,
    stopping: Arc<AtomicBool>,
) where
    R: RecycleBin + Send + 'static,
    P: ExternalMetadataProvider,
{
    while !stopping.load(Ordering::Acquire) {
        let due_job = match application.lock() {
            Ok(application) => match application.due_external_search_jobs(1) {
                Ok(jobs) => jobs.into_iter().next(),
                Err(error) => {
                    eprintln!("external search worker 無法讀取工作：{error}");
                    None
                }
            },
            Err(_) => {
                eprintln!("external search worker 無法取得 application service");
                return;
            }
        };
        let Some(due_job) = due_job else {
            thread::sleep(WORKER_POLL_INTERVAL);
            continue;
        };

        let request = match application.lock() {
            Ok(mut application) => match application.start_external_search_job(due_job.id) {
                Ok(ExternalSearchStartOutcome::Ready(request)) => request,
                Ok(ExternalSearchStartOutcome::Finished(_)) => continue,
                Err(error) => {
                    eprintln!("external search job {} 無法開始：{error}", due_job.id);
                    continue;
                }
            },
            Err(_) => {
                eprintln!("external search worker 無法取得 application service");
                return;
            }
        };

        let response = provider.search(&request);
        let finish_error = match application.lock() {
            Ok(mut application) => application
                .finish_external_search_job(due_job.id, response)
                .err()
                .map(|error| error.to_string()),
            Err(_) => {
                eprintln!("external search worker 無法取得 application service");
                return;
            }
        };
        if let Some(message) = finish_error {
            eprintln!("external search job {} 無法寫回：{message}", due_job.id);
            match application.lock() {
                Ok(mut application) => {
                    if let Err(error) =
                        application.requeue_interrupted_external_search_job(due_job.id, &message)
                    {
                        eprintln!("external search job {} 無法回復：{error}", due_job.id);
                    }
                }
                Err(_) => {
                    eprintln!("external search worker 無法取得 application service");
                    return;
                }
            }
        }
    }
}

fn usage_and_exit() -> ! {
    eprintln!("用法：doujin-http <v2-catalog.db> [port]");
    std::process::exit(64);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[derive(Debug, Clone, Copy)]
    struct NoopRecycleBin;

    impl RecycleBin for NoopRecycleBin {
        fn recycle(&self, _path: &Path) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn provider_chain_failure_keeps_both_sources_and_retryable_kind() {
        let combined = combine_provider_errors(
            ExternalSearchProviderError {
                kind: doujin_storage::jobs::ExternalSearchErrorKind::NoMatch,
                message: "找不到 E-Hentai gallery".to_owned(),
            },
            ExternalSearchProviderError {
                kind: doujin_storage::jobs::ExternalSearchErrorKind::Network,
                message: "DLsite 暫時無法連線".to_owned(),
            },
        );

        assert_eq!(
            doujin_storage::jobs::ExternalSearchErrorKind::Network,
            combined.kind
        );
        assert!(combined.message.contains("找不到 E-Hentai gallery"));
        assert!(combined.message.contains("DLsite 暫時無法連線"));
    }

    #[test]
    fn production_thumbnail_claim_does_not_enqueue_or_run_background_prewarm() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "doujin-http-explicit-thumbnail-{}-{unique}",
            std::process::id()
        ));
        let library = base.join("library");
        fs::create_dir_all(&library).expect("create library");
        fs::write(library.join("[circle] untracked.zip"), b"zip placeholder")
            .expect("create source");
        let repository = CatalogRepository::open_in_memory().expect("open catalog");
        let thumbnail_config =
            ThumbnailConfig::new(base.join("cache"), 300, 400, 80).expect("thumbnail config");
        let mut application =
            ApplicationService::with_thumbnails(repository, NoopRecycleBin, thumbnail_config);
        application
            .run_scan(&[doujin_scanner::ScanRoot {
                path: library,
                source: doujin_scanner::SourceKind::Downloads,
                label: "下載區".to_owned(),
            }])
            .expect("scan source");

        assert!(
            claim_next_queued_thumbnail(&mut application)
                .expect("claim queued thumbnail")
                .is_none(),
            "production claim must not turn an untracked collection into background work"
        );
        assert!(
            application
                .repository()
                .next_untracked_thumbnail_collection_id()
                .expect("untracked thumbnail lookup")
                .is_some()
        );

        application
            .prewarm_next_thumbnail()
            .expect("enqueue background prewarm")
            .expect("background thumbnail");
        assert!(
            claim_next_queued_thumbnail(&mut application)
                .expect("ignore background thumbnail")
                .is_none(),
            "production worker must not claim persisted background prewarm"
        );

        drop(application);
        fs::remove_dir_all(base).expect("remove test tree");
    }

    #[test]
    fn startup_settings_follow_environment_then_database_then_file_then_defaults() {
        let base = std::env::temp_dir().join("doujin-http-settings-precedence");
        let database = base.join("catalog.db");
        let config_directory = base.join("config");
        let file = || FileSettings {
            viewer_path: Some(PathBuf::from("file-reader.exe")),
            thumb_dir: Some(PathBuf::from("file-cache")),
            thumb_size: Some("200x300".to_owned()),
            thumb_quality: Some(60),
        };
        let stored = || StoredApplicationSettings {
            reader_path: Some(base.join("stored-reader.exe")),
            thumbnail_width: 300,
            thumbnail_height: 400,
            thumbnail_quality: 70,
            updated_at: "2026-08-12T00:00:00.000Z".to_owned(),
        };
        let environment = EnvironmentSettings {
            reader_path: Some(base.join("environment-reader.exe")),
            thumbnail_dir: Some(base.join("environment-cache")),
            thumbnail_size: Some("500x600".to_owned()),
            thumbnail_quality: Some("90".to_owned()),
        };

        let environment_result = resolve_startup_settings(
            &database,
            &config_directory,
            file(),
            Some(stored()),
            environment,
        )
        .expect("environment settings");
        assert_eq!(
            Some(base.join("environment-reader.exe")),
            environment_result.reader_path
        );
        assert_eq!(
            base.join("environment-cache"),
            environment_result.thumbnail_config.cache_dir
        );
        assert_eq!(500, environment_result.thumbnail_config.width);
        assert_eq!(600, environment_result.thumbnail_config.height);
        assert_eq!(90, environment_result.thumbnail_config.quality);
        assert!(environment_result.overrides.reader_path.is_some());

        let stored_result = resolve_startup_settings(
            &database,
            &config_directory,
            file(),
            Some(stored()),
            EnvironmentSettings::default(),
        )
        .expect("stored settings");
        assert_eq!(
            Some(base.join("stored-reader.exe")),
            stored_result.reader_path
        );
        assert_eq!(300, stored_result.thumbnail_config.width);
        assert_eq!(400, stored_result.thumbnail_config.height);
        assert_eq!(70, stored_result.thumbnail_config.quality);
        assert_eq!(
            config_directory.join("file-cache"),
            stored_result.thumbnail_config.cache_dir
        );

        let file_result = resolve_startup_settings(
            &database,
            &config_directory,
            file(),
            None,
            EnvironmentSettings::default(),
        )
        .expect("file settings");
        assert_eq!(
            Some(config_directory.join("file-reader.exe")),
            file_result.reader_path
        );
        assert_eq!(200, file_result.thumbnail_config.width);
        assert_eq!(300, file_result.thumbnail_config.height);
        assert_eq!(60, file_result.thumbnail_config.quality);

        let default_result = resolve_startup_settings(
            &database,
            &config_directory,
            FileSettings::default(),
            None,
            EnvironmentSettings::default(),
        )
        .expect("default settings");
        assert_eq!(None, default_result.reader_path);
        assert_eq!(300, default_result.thumbnail_config.width);
        assert_eq!(400, default_result.thumbnail_config.height);
        assert_eq!(80, default_result.thumbnail_config.quality);
        assert_eq!(
            base.join("catalog.db.thumbnails"),
            default_result.thumbnail_config.cache_dir
        );
    }

    #[test]
    fn startup_rejects_invalid_thumbnail_file_settings() {
        let base = std::env::temp_dir().join("doujin-http-invalid-settings");
        let result = resolve_startup_settings(
            &base.join("catalog.db"),
            &base,
            FileSettings {
                thumb_size: Some("300*400".to_owned()),
                ..FileSettings::default()
            },
            None,
            EnvironmentSettings::default(),
        );
        assert!(result.is_err());
    }
}
