#![cfg_attr(windows, windows_subsystem = "windows")]
//! 原生視窗版私藏編目室：同 process 跑 doujin-http 服務，關閉視窗即結束整個程式。

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use doujin_desktop::{describe_start_failure, read_catalog, store_catalog, window_url};
use doujin_http::{ServiceInstanceConfig, ServiceOptions, run_service};
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};

const CREATE_CATALOG: &str = "建立新的 catalog";
const OPEN_CATALOG: &str = "開啟既有 catalog";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const FAILURE_DETAIL_TIMEOUT: Duration = Duration::from_secs(5);
const MAIN_WINDOW: &str = "main";

/// 服務執行緒的兩個把手：送出 shutdown 訊號，以及等待 `run_service` 收工的回報。
struct ServiceHandle {
    shutdown: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    finished: Mutex<Receiver<Result<(), String>>>,
}

static SERVICE: OnceLock<ServiceHandle> = OnceLock::new();

fn main() {
    let application = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |app, _arguments, _cwd| {
                focus_main_window(app);
            },
        ))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            thread::Builder::new()
                .name("desktop-startup".to_owned())
                .spawn(move || {
                    if let Err(error) = start(&handle) {
                        fail(&handle, &error);
                    }
                })?;
            Ok(())
        })
        .build(tauri::generate_context!());
    let application = match application {
        Ok(application) => application,
        Err(error) => {
            eprintln!("無法開啟私藏編目室：{error}");
            std::process::exit(1);
        }
    };
    application.run(|_handle, event| {
        if matches!(event, RunEvent::Exit) {
            stop_service();
        }
    });
}

/// 啟動流程：解析 catalog → 啟動服務 → 等綁定位址 → 建立視窗。全程在背景執行緒進行，
/// 這樣原生對話框的 blocking API 才不會擋住主執行緒的事件迴圈。
fn start(app: &AppHandle) -> Result<(), String> {
    let state_directory = doujin_launcher::state_directory()
        .map_err(|error| format!("無法解析狀態資料夾：{error}"))?;
    fs::create_dir_all(&state_directory)
        .map_err(|error| format!("無法建立狀態資料夾 {}：{error}", state_directory.display()))?;
    let catalog = match read_catalog(&state_directory) {
        Some(catalog) => catalog,
        None => match choose_catalog(app)? {
            Some(catalog) => catalog,
            None => {
                // 取消首次啟動：不寫任何狀態，直接結束。
                app.exit(0);
                return Ok(());
            }
        },
    };
    doujin_launcher::validate_catalog_destination(&catalog)
        .map_err(|error| format!("catalog 無法使用：{error}"))?;
    store_catalog(&state_directory, &catalog)
        .map_err(|error| format!("無法記錄 catalog 選擇：{error}"))?;
    let address = start_service(&state_directory, &catalog)?;
    let onboarding = doujin_launcher::onboarding_required(address.port()).unwrap_or(false);
    open_window(app, &window_url(address, onboarding), address.port())
}

/// 首次啟動：先問要建立還是開啟，再用原生檔案對話框選路徑。
/// 取消訊息對話框或取消檔案對話框都代表放棄啟動，回傳 `None`。
fn choose_catalog(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    let choice = app
        .dialog()
        .message(
            "選擇「建立新的 catalog」開始全新的編目資料庫，或選擇「開啟既有 catalog」沿用現有的 .db 檔。",
        )
        .title("私藏編目室首次啟動")
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            CREATE_CATALOG.to_owned(),
            OPEN_CATALOG.to_owned(),
            "取消".to_owned(),
        ))
        .blocking_show_with_result();
    let create = match choice {
        MessageDialogResult::Custom(label) if label == CREATE_CATALOG => true,
        MessageDialogResult::Custom(label) if label == OPEN_CATALOG => false,
        MessageDialogResult::Yes => true,
        MessageDialogResult::No => false,
        _ => return Ok(None),
    };
    let selected = if create {
        app.dialog()
            .file()
            .set_title("建立私藏編目室 catalog")
            .add_filter("Doujin catalog", &["db"])
            .set_file_name("catalog.db")
            .blocking_save_file()
    } else {
        app.dialog()
            .file()
            .set_title("選擇既有 v2 catalog")
            .add_filter("Doujin catalog", &["db"])
            .blocking_pick_file()
    };
    selected
        .map(|path| {
            path.into_path()
                .map_err(|error| format!("無法解析選取的 catalog 路徑：{error}"))
        })
        .transpose()
}

/// 在專屬執行緒的 tokio runtime 上跑 `run_service`，回傳實際綁定的 loopback 位址。
fn start_service(state_directory: &Path, catalog: &Path) -> Result<SocketAddr, String> {
    let options = ServiceOptions {
        database: catalog.to_path_buf(),
        port: 0,
        config_path: state_directory.join("config.json"),
        instance: Some(ServiceInstanceConfig {
            metadata_path: state_directory.join(doujin_launcher::INSTANCE_FILE),
            lock_path: state_directory.join(doujin_launcher::SERVICE_LOCK_FILE),
            instance_id: doujin_launcher::new_instance_id(),
        }),
    };
    let (ready_sender, ready_receiver) = mpsc::channel();
    let (finished_sender, finished_receiver) = mpsc::channel();
    let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel();
    thread::Builder::new()
        .name("doujin-service".to_owned())
        .spawn(move || {
            let result = run_service_blocking(options, ready_sender, shutdown_receiver);
            let _ = finished_sender.send(result);
        })
        .map_err(|error| format!("無法啟動服務執行緒：{error}"))?;

    match ready_receiver.recv_timeout(START_TIMEOUT) {
        Ok(address) => {
            let _ = SERVICE.set(ServiceHandle {
                shutdown: Mutex::new(Some(shutdown_sender)),
                finished: Mutex::new(finished_receiver),
            });
            Ok(address)
        }
        // on_ready 還沒被呼叫，服務執行緒就結束了：實際原因在 finished channel 上。
        Err(RecvTimeoutError::Disconnected) => Err(
            match finished_receiver.recv_timeout(FAILURE_DETAIL_TIMEOUT) {
                Ok(Err(message)) => describe_start_failure(&message),
                Ok(Ok(())) => "服務尚未開始接受請求就結束了".to_owned(),
                Err(_) => "服務啟動失敗，且沒有回報原因".to_owned(),
            },
        ),
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "服務啟動逾時（{} 秒）；請確認 catalog 權限與 migration 版本",
            START_TIMEOUT.as_secs()
        )),
    }
}

fn run_service_blocking(
    options: ServiceOptions,
    ready: mpsc::Sender<SocketAddr>,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("無法建立 async runtime：{error}"))?;
    runtime
        .block_on(run_service(
            options,
            move |address| {
                let _ = ready.send(address);
            },
            async move {
                let _ = shutdown.await;
            },
        ))
        .map_err(|error| error.to_string())
}

/// 視窗只能由主執行緒建立，所以把建立動作丟回事件迴圈並等它回報結果。
fn open_window(app: &AppHandle, url: &str, port: u16) -> Result<(), String> {
    let url = Url::parse(url).map_err(|error| format!("視窗 URL 無效：{error}"))?;
    let (sender, receiver) = mpsc::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = sender.send(build_window(&handle, url, port).map_err(|error| error.to_string()));
    })
    .map_err(|error| format!("無法建立視窗：{error}"))?;
    receiver
        .recv()
        .map_err(|_| "視窗建立過程中斷".to_owned())?
        .map_err(|error| format!("無法建立視窗：{error}"))
}

fn build_window(app: &AppHandle, url: Url, port: u16) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::External(url))
        .title("私藏編目室")
        .inner_size(1280.0, 800.0)
        .min_inner_size(960.0, 600.0)
        .resizable(true)
        .maximizable(true)
        // WebView 只停留在自己的 loopback origin；其他位址一律拒絕導覽。
        .on_navigation(move |url| {
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port() == Some(port)
        })
        .build()
        .map(|_| ())
}

/// 視窗關閉後的收尾：送出 shutdown、有界等待 `run_service` 收工，逾時就強制結束。
fn stop_service() {
    let Some(service) = SERVICE.get() else {
        return;
    };
    if let Ok(mut shutdown) = service.shutdown.lock()
        && let Some(sender) = shutdown.take()
    {
        let _ = sender.send(());
    }
    let Ok(finished) = service.finished.lock() else {
        return;
    };
    match finished.recv_timeout(SHUTDOWN_TIMEOUT) {
        Ok(Ok(())) | Err(RecvTimeoutError::Disconnected) => {}
        Ok(Err(message)) => eprintln!("服務結束時回報錯誤：{message}"),
        Err(RecvTimeoutError::Timeout) => {
            eprintln!("服務未在 {} 秒內收工，強制結束", SHUTDOWN_TIMEOUT.as_secs());
            std::process::exit(0);
        }
    }
}

fn focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn fail(app: &AppHandle, message: &str) -> ! {
    app.dialog()
        .message(format!("無法開啟私藏編目室：{message}"))
        .title("私藏編目室")
        .kind(MessageDialogKind::Error)
        .blocking_show();
    std::process::exit(1);
}
