//! Windows-first bootstrap and lifecycle support for the localhost catalog service.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = "launcher.json";
const INSTANCE_FILE: &str = "instance.json";
const LAUNCH_LOCK_FILE: &str = "launcher.lock";
const SERVICE_LOCK_FILE: &str = "service.lock";
const START_TIMEOUT: Duration = Duration::from_secs(20);
static INSTANCE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LauncherCommand {
    Open { catalog: Option<PathBuf> },
    Status,
    Restart,
    Stop,
    Help,
}

#[derive(Debug, Serialize, Deserialize)]
struct LauncherConfig {
    catalog: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceMetadata {
    pub schema: u8,
    pub pid: u32,
    pub catalog: PathBuf,
    pub port: u16,
    pub instance_id: String,
    pub started_at_unix: u64,
}

impl InstanceMetadata {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}

pub fn parse_arguments<I, S>(arguments: I) -> Result<LauncherCommand, String>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut arguments = arguments.into_iter().map(Into::into);
    let first = arguments.next();
    match first.as_deref().and_then(OsStr::to_str) {
        None | Some("open") => parse_open_arguments(arguments),
        Some("status") => no_more(arguments, LauncherCommand::Status),
        Some("restart") => no_more(arguments, LauncherCommand::Restart),
        Some("stop") => no_more(arguments, LauncherCommand::Stop),
        Some("help" | "--help" | "-h") => no_more(arguments, LauncherCommand::Help),
        Some(other) => Err(format!("未知 launcher command：{other}")),
    }
}

fn parse_open_arguments<I>(mut arguments: I) -> Result<LauncherCommand, String>
where
    I: Iterator<Item = OsString>,
{
    let mut catalog = None;
    while let Some(argument) = arguments.next() {
        if argument != "--catalog" {
            return Err(format!("未知 open option：{}", argument.to_string_lossy()));
        }
        let path = arguments
            .next()
            .ok_or_else(|| "--catalog 需要 catalog 路徑".to_owned())?;
        if catalog.replace(PathBuf::from(path)).is_some() {
            return Err("--catalog 只能指定一次".to_owned());
        }
    }
    Ok(LauncherCommand::Open { catalog })
}

fn no_more<I>(mut arguments: I, command: LauncherCommand) -> Result<LauncherCommand, String>
where
    I: Iterator<Item = OsString>,
{
    if let Some(argument) = arguments.next() {
        Err(format!("不接受額外參數：{}", argument.to_string_lossy()))
    } else {
        Ok(command)
    }
}

pub struct Launcher {
    state_directory: PathBuf,
    http_executable: PathBuf,
}

impl Launcher {
    pub fn discover() -> Result<Self, Box<dyn std::error::Error>> {
        let state_directory = state_directory()?;
        let http_executable = match std::env::var_os("DOUJIN_HTTP_EXECUTABLE") {
            Some(path) => PathBuf::from(path),
            None => std::env::current_exe()?
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(http_executable_name()),
        };
        Ok(Self {
            state_directory,
            http_executable,
        })
    }

    pub fn execute(&self, command: LauncherCommand) -> Result<String, Box<dyn std::error::Error>> {
        fs::create_dir_all(&self.state_directory)?;
        match command {
            LauncherCommand::Open { catalog } => {
                let catalog = self.resolve_catalog(catalog)?;
                self.with_launch_lock(|| self.open_catalog(&catalog))
            }
            LauncherCommand::Status => self.status(),
            LauncherCommand::Restart => self.with_launch_lock(|| {
                self.stop_selected()?;
                let catalog = self.selected_catalog()?;
                self.open_catalog(&catalog)
            }),
            LauncherCommand::Stop => self.with_launch_lock(|| self.stop_selected()),
            LauncherCommand::Help => Ok(usage().to_owned()),
        }
    }

    fn resolve_catalog(
        &self,
        requested: Option<PathBuf>,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let catalog = if let Some(path) = requested {
            absolute_path(path)?
        } else if let Ok(config) = self.read_config() {
            config.catalog
        } else {
            choose_first_catalog(&self.state_directory)?
        };
        validate_catalog_destination(&catalog)?;
        write_json_atomic(
            &self.state_directory.join(CONFIG_FILE),
            &LauncherConfig {
                catalog: catalog.clone(),
            },
        )?;
        Ok(catalog)
    }

    fn selected_catalog(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        Ok(self.read_config()?.catalog)
    }

    fn read_config(&self) -> Result<LauncherConfig, Box<dyn std::error::Error>> {
        Ok(serde_json::from_slice(&fs::read(
            self.state_directory.join(CONFIG_FILE),
        )?)?)
    }

    fn with_launch_lock<T>(
        &self,
        operation: impl FnOnce() -> Result<T, Box<dyn std::error::Error>>,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.state_directory.join(LAUNCH_LOCK_FILE))?;
        lock.lock_exclusive()?;
        let result = operation();
        let _ = lock.unlock();
        result
    }

    fn open_catalog(&self, catalog: &Path) -> Result<String, Box<dyn std::error::Error>> {
        let metadata_path = self.state_directory.join(INSTANCE_FILE);
        let existing = match read_instance(&metadata_path) {
            Ok(instance) => instance,
            Err(_) if metadata_path.exists() => {
                fs::remove_file(&metadata_path)?;
                None
            }
            Err(error) => return Err(error),
        };
        if let Some(instance) = existing {
            if instance.catalog == catalog
                && health_check(instance.port, Some(&instance.instance_id)).is_ok()
            {
                open_browser(&browser_url(&instance))?;
                return Ok(format!(
                    "編目室已在執行：{}（catalog：{}）",
                    instance.url(),
                    instance.catalog.display()
                ));
            }
            // Metadata can outlive a crashed service, and its PID can later be
            // reused by an unrelated process. Never kill that PID from `open`;
            // remove the stale claim and let the independent service lock decide
            // whether an old doujin-http still owns this catalog.
            fs::remove_file(&metadata_path)?;
        }
        if !self.http_executable.is_file() {
            return Err(format!(
                "找不到 doujin-http：{}；請先執行 Windows build/install script",
                self.http_executable.display()
            )
            .into());
        }
        let port = available_loopback_port()?;
        let instance_id = new_instance_id();
        let mut child = self.spawn_http(catalog, port, &instance_id, &metadata_path)?;
        let instance = wait_for_service(
            &mut child,
            catalog,
            &instance_id,
            &metadata_path,
            START_TIMEOUT,
        )?;
        open_browser(&browser_url(&instance))?;
        Ok(format!(
            "編目室已開啟：{}（catalog：{}）",
            instance.url(),
            instance.catalog.display()
        ))
    }

    fn spawn_http(
        &self,
        catalog: &Path,
        port: u16,
        instance_id: &str,
        metadata_path: &Path,
    ) -> Result<Child, Box<dyn std::error::Error>> {
        let log_path = self.state_directory.join("service.log");
        let error_path = self.state_directory.join("service-error.log");
        let stdout = File::create(log_path)?;
        let stderr = File::create(error_path)?;
        let mut command = Command::new(&self.http_executable);
        command
            .arg(catalog)
            .arg(port.to_string())
            .env("DOUJIN_INSTANCE_METADATA", metadata_path)
            .env("DOUJIN_INSTANCE_ID", instance_id)
            .env(
                "DOUJIN_INSTANCE_LOCK",
                self.state_directory.join(SERVICE_LOCK_FILE),
            )
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        configure_detached_process(&mut command);
        command.spawn().map_err(|error| {
            io::Error::new(error.kind(), format!("無法啟動 doujin-http：{error}")).into()
        })
    }

    fn status(&self) -> Result<String, Box<dyn std::error::Error>> {
        let catalog = self.selected_catalog()?;
        let Some(instance) = read_instance(&self.state_directory.join(INSTANCE_FILE))? else {
            return Ok(format!("服務未執行（catalog：{}）", catalog.display()));
        };
        if health_check(instance.port, Some(&instance.instance_id)).is_ok() {
            Ok(format!(
                "服務執行中 · catalog：{} · port：{} · PID：{}",
                instance.catalog.display(),
                instance.port,
                instance.pid
            ))
        } else {
            Ok(format!(
                "服務狀態異常 · catalog：{} · port：{} · PID：{}",
                instance.catalog.display(),
                instance.port,
                instance.pid
            ))
        }
    }

    fn stop_selected(&self) -> Result<String, Box<dyn std::error::Error>> {
        let metadata_path = self.state_directory.join(INSTANCE_FILE);
        let Some(instance) = read_instance(&metadata_path)? else {
            return Ok("服務目前未執行".to_owned());
        };
        let selected_catalog = self.selected_catalog()?;
        let identity_matches = health_check(instance.port, Some(&instance.instance_id)).is_ok();
        match stop_disposition(
            &instance,
            &selected_catalog,
            identity_matches,
            process_is_running(instance.pid)?,
        )? {
            StopDisposition::CleanStale => {
                fs::remove_file(metadata_path)?;
                return Ok("已清理停止服務留下的舊狀態".to_owned());
            }
            StopDisposition::Stop => {}
        }
        stop_process(instance.pid)?;
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline && process_is_running(instance.pid)? {
            thread::sleep(Duration::from_millis(100));
        }
        if process_is_running(instance.pid)? {
            return Err(format!("無法停止服務程序 {}", instance.pid).into());
        }
        if metadata_path.exists() {
            fs::remove_file(metadata_path)?;
        }
        Ok(format!(
            "服務已停止 · catalog：{} · port：{}",
            instance.catalog.display(),
            instance.port
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopDisposition {
    CleanStale,
    Stop,
}

fn stop_disposition(
    instance: &InstanceMetadata,
    selected_catalog: &Path,
    identity_matches: bool,
    process_running: bool,
) -> Result<StopDisposition, Box<dyn std::error::Error>> {
    if instance.catalog != selected_catalog {
        return Err(format!(
            "instance catalog 與目前選取的 catalog 不同（{} / {}）；未停止任何程序",
            instance.catalog.display(),
            selected_catalog.display()
        )
        .into());
    }
    if identity_matches {
        return Ok(StopDisposition::Stop);
    }
    if !process_running {
        return Ok(StopDisposition::CleanStale);
    }
    Err(format!(
        "無法核對服務程序 {} 的 catalog、port、health 與 instance identity；為避免 PID 重用誤殺，未停止任何程序",
        instance.pid
    )
    .into())
}

fn wait_for_service(
    child: &mut Child,
    catalog: &Path,
    instance_id: &str,
    metadata_path: &Path,
    timeout: Duration,
) -> Result<InstanceMetadata, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            let details = metadata_path
                .parent()
                .map(|directory| directory.join("service-error.log"))
                .and_then(|path| fs::read_to_string(path).ok())
                .unwrap_or_default();
            let details = details.trim();
            return Err(if details.is_empty() {
                format!("doujin-http 啟動失敗（exit {status}）")
            } else {
                format!("doujin-http 啟動失敗：{details}")
            }
            .into());
        }
        if let Some(instance) = read_instance(metadata_path)? {
            if instance.catalog != catalog {
                return Err("service 回報了不同的 catalog；已停止啟動以保護資料".into());
            }
            if instance.instance_id != instance_id {
                return Err("service 回報了不同的 instance identity；已停止啟動".into());
            }
            if health_check(instance.port, Some(instance_id)).is_ok() {
                return Ok(instance);
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(
        "服務啟動逾時；請查看 service-error.log，確認 catalog 權限、migration version 與 port 狀態"
            .into(),
    )
}

pub fn health_check(port: u16, expected_instance_id: Option<&str>) -> io::Result<()> {
    let health = loopback_json(port, "/api/health")?;
    if health["status"] != "ok" || health["service"] != "doujin-http" {
        return Err(io::Error::other("port 上不是相容的 doujin-http service"));
    }
    if expected_instance_id.is_some_and(|expected| health["instance_id"] != expected) {
        return Err(io::Error::other("service instance identity 不相符"));
    }
    Ok(())
}

fn browser_url(instance: &InstanceMetadata) -> String {
    if onboarding_required(instance.port).unwrap_or(false) {
        format!("{}#settings", instance.url())
    } else {
        instance.url()
    }
}

fn onboarding_required(port: u16) -> io::Result<bool> {
    let roots = loopback_json(port, "/api/library-roots")?;
    let roots = roots["roots"]
        .as_array()
        .ok_or_else(|| io::Error::other("library roots response 格式無效"))?;
    let has_downloads = roots
        .iter()
        .any(|root| root["active"] == true && root["source"] == "downloads");
    let has_archive = roots
        .iter()
        .any(|root| root["active"] == true && root["source"] == "archive");
    Ok(!has_downloads || !has_archive)
}

fn loopback_json(port: u16, path: &str) -> io::Result<serde_json::Value> {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if !response.starts_with("HTTP/1.1 200") {
        return Err(io::Error::other("port 上不是相容的 doujin-http service"));
    }
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .ok_or_else(|| io::Error::other("health response 格式無效"))?;
    serde_json::from_str(body).map_err(io::Error::other)
}

fn new_instance_id() -> String {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = INSTANCE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{timestamp:x}-{sequence:x}", std::process::id())
}

pub fn available_loopback_port() -> io::Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.local_addr().map(|address| address.port())
}

fn read_instance(path: &Path) -> Result<Option<InstanceMetadata>, Box<dyn std::error::Error>> {
    match fs::read(path) {
        Ok(bytes) => {
            let instance: InstanceMetadata = serde_json::from_slice(&bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("instance metadata 無法解析：{error}"),
                )
            })?;
            if instance.schema != 1
                || instance.port == 0
                || instance.instance_id.is_empty()
                || !instance.catalog.is_absolute()
            {
                return Err("instance metadata 內容無效".into());
            }
            Ok(Some(instance))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn validate_catalog_destination(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_absolute() {
        return Err("catalog path 必須是絕對路徑".into());
    }
    if path.exists() && !path.is_file() {
        return Err(format!("catalog 不是一般檔案：{}", path.display()).into());
    }
    let parent = path.parent().ok_or("catalog path 缺少父資料夾")?;
    if !parent.is_dir() {
        return Err(format!("catalog 父資料夾不存在：{}", parent.display()).into());
    }
    Ok(())
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(io::Error::other)?,
    )?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temporary, path)
}

fn absolute_path(path: PathBuf) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn state_directory() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("DOUJIN_LAUNCHER_STATE_DIR") {
        return absolute_path(PathBuf::from(path));
    }
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA")) {
        return Ok(PathBuf::from(path).join("Doujin Tagger"));
    }
    Ok(std::env::current_dir()?.join(".doujin-tagger"))
}

fn choose_first_catalog(state_directory: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    #[cfg(windows)]
    if let Some(path) = windows_catalog_dialog()? {
        return Ok(path);
    }
    let default = state_directory.join("catalog.db");
    fs::create_dir_all(state_directory)?;
    Ok(default)
}

#[cfg(windows)]
fn windows_catalog_dialog() -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
$choice = [System.Windows.Forms.MessageBox]::Show('選擇「是」建立新的 catalog；選擇「否」開啟既有 catalog。', '私藏編目室首次啟動', 'YesNoCancel', 'Information')
if ($choice -eq 'Cancel') { exit 2 }
if ($choice -eq 'Yes') {
  $dialog = New-Object System.Windows.Forms.SaveFileDialog
  $dialog.Title = '建立私藏編目室 catalog'
  $dialog.Filter = 'Doujin catalog (*.db)|*.db'
  $dialog.FileName = 'catalog.db'
} else {
  $dialog = New-Object System.Windows.Forms.OpenFileDialog
  $dialog.Title = '選擇既有 v2 catalog'
  $dialog.Filter = 'Doujin catalog (*.db)|*.db|All files (*.*)|*.*'
  $dialog.CheckFileExists = $true
}
if ($dialog.ShowDialog() -ne 'OK') { exit 2 }
[Console]::Out.Write($dialog.FileName)
"#;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", script])
        .output()?;
    if output.status.code() == Some(2) {
        return Err("已取消首次啟動；尚未建立或變更 catalog".into());
    }
    if !output.status.success() {
        return Ok(None);
    }
    let path = String::from_utf8(output.stdout)?.trim().to_owned();
    Ok((!path.is_empty()).then(|| PathBuf::from(path)))
}

fn open_browser(url: &str) -> io::Result<()> {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("rundll32.exe");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().map(|_| ())
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> io::Result<bool> {
    let filter = format!("PID eq {pid}");
    let output = Command::new("tasklist.exe")
        .args(["/FI", &filter, "/FO", "CSV", "/NH"])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(output.status.success()
        && text
            .lines()
            .any(|line| line.contains(&format!("\"{pid}\""))))
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> io::Result<bool> {
    Ok(Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}

#[cfg(windows)]
fn stop_process(pid: u32) -> io::Result<()> {
    let status = Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/T"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("taskkill 無法停止服務"))
    }
}

#[cfg(unix)]
fn stop_process(pid: u32) -> io::Result<()> {
    let status = Command::new("kill").arg(pid.to_string()).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("無法停止服務"))
    }
}

#[cfg(windows)]
fn configure_detached_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_detached_process(_command: &mut Command) {}

#[cfg(windows)]
fn http_executable_name() -> &'static str {
    "doujin-http.exe"
}

#[cfg(not(windows))]
fn http_executable_name() -> &'static str {
    "doujin-http"
}

pub fn usage() -> &'static str {
    "用法：doujin-launcher [open [--catalog <v2-catalog.db>] | status | restart | stop | help]"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_means_open_selected_catalog() {
        assert_eq!(
            LauncherCommand::Open { catalog: None },
            parse_arguments(Vec::<OsString>::new()).expect("arguments")
        );
    }

    #[test]
    fn open_accepts_an_explicit_catalog() {
        assert_eq!(
            LauncherCommand::Open {
                catalog: Some(PathBuf::from("D:/catalog.db"))
            },
            parse_arguments(["open", "--catalog", "D:/catalog.db"]).expect("arguments")
        );
    }

    #[test]
    fn lifecycle_commands_reject_extra_arguments() {
        assert!(parse_arguments(["status", "extra"]).is_err());
        assert!(parse_arguments(["restart", "extra"]).is_err());
        assert!(parse_arguments(["stop", "extra"]).is_err());
    }

    #[test]
    fn selected_port_is_ephemeral_and_loopback_only() {
        let port = available_loopback_port().expect("available port");
        assert_ne!(0, port);
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).expect("port released after selection");
    }

    #[test]
    fn instance_metadata_round_trips() {
        let directory = test_directory("metadata");
        fs::create_dir_all(&directory).expect("directory");
        let path = directory.join(INSTANCE_FILE);
        let expected = InstanceMetadata {
            schema: 1,
            pid: 42,
            catalog: directory.join("catalog.db"),
            port: 5000,
            instance_id: "instance-42".to_owned(),
            started_at_unix: 10,
        };
        write_json_atomic(&path, &expected).expect("write");
        assert_eq!(Some(expected), read_instance(&path).expect("read"));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn invalid_instance_metadata_is_rejected() {
        let directory = test_directory("bad-metadata");
        fs::create_dir_all(&directory).expect("directory");
        let path = directory.join(INSTANCE_FILE);
        fs::write(
            &path,
            br#"{"schema":1,"pid":42,"catalog":"relative.db","port":0,"instance_id":"bad","started_at_unix":10}"#,
        )
        .expect("write");
        assert!(read_instance(&path).is_err());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn health_check_requires_the_expected_instance_identity() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let port = listener.local_addr().expect("address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request).expect("request");
            let body = r#"{"status":"ok","service":"doujin-http","api_version":1,"instance_id":"expected"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("response");
        });
        health_check(port, Some("expected")).expect("matching identity");
        server.join().expect("server");

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let port = listener.local_addr().expect("address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request).expect("request");
            let body =
                r#"{"status":"ok","service":"doujin-http","api_version":1,"instance_id":"other"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("response");
        });
        assert!(health_check(port, Some("expected")).is_err());
        server.join().expect("server");
    }

    #[test]
    fn pid_reuse_cannot_authorize_a_stop() {
        let directory = test_directory("pid-reuse");
        let instance = InstanceMetadata {
            schema: 1,
            pid: 42,
            catalog: directory.join("catalog.db"),
            port: 5000,
            instance_id: "old-instance".to_owned(),
            started_at_unix: 10,
        };
        assert!(stop_disposition(&instance, &instance.catalog, false, true).is_err());
        assert_eq!(
            StopDisposition::CleanStale,
            stop_disposition(&instance, &instance.catalog, false, false).expect("stale")
        );
        assert_eq!(
            StopDisposition::Stop,
            stop_disposition(&instance, &instance.catalog, true, true).expect("verified")
        );
        assert!(stop_disposition(&instance, &directory.join("other.db"), true, true).is_err());
    }

    #[test]
    fn launch_and_service_locks_do_not_deadlock_each_other() {
        let directory = test_directory("independent-locks");
        fs::create_dir_all(&directory).expect("directory");
        let launcher = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(LAUNCH_LOCK_FILE))
            .expect("launcher lock");
        launcher.lock_exclusive().expect("lock launcher");
        let service = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(SERVICE_LOCK_FILE))
            .expect("service lock");
        service
            .try_lock_exclusive()
            .expect("service lock remains independent");
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(windows)]
    #[test]
    fn windows_launcher_discovers_the_sibling_http_executable() {
        assert_eq!("doujin-http.exe", http_executable_name());
    }

    fn test_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "doujin-launcher-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }
}
