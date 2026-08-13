//! 桌面版的純邏輯縫：與 launcher 共用的 catalog 設定讀寫、視窗 URL 組裝與啟動錯誤訊息。

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use doujin_launcher::{CONFIG_FILE, LauncherConfig, write_json_atomic};

/// 讀取 state 目錄內 launcher 與桌面版共用的 `launcher.json`。
///
/// 檔案不存在、無法讀取或內容無效都回傳 `None`，代表要走首次啟動流程——
/// 與 launcher `open` 的處置相同。
pub fn read_catalog(state_directory: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(state_directory.join(CONFIG_FILE)).ok()?;
    let config: LauncherConfig = serde_json::from_slice(&bytes).ok()?;
    Some(config.catalog)
}

/// 以原子寫入更新 `launcher.json`，格式與 launcher 寫出的完全相同。
pub fn store_catalog(state_directory: &Path, catalog: &Path) -> io::Result<()> {
    write_json_atomic(
        &state_directory.join(CONFIG_FILE),
        &LauncherConfig {
            catalog: catalog.to_path_buf(),
        },
    )
}

/// 組出視窗要載入的 loopback URL；需要 onboarding 時附上 `#settings` fragment。
pub fn window_url(address: SocketAddr, onboarding: bool) -> String {
    let base = format!("http://127.0.0.1:{}/", address.port());
    if onboarding {
        format!("{base}#settings")
    } else {
        base
    }
}

/// 把服務啟動失敗的原始訊息轉成使用者看得懂的說明；service lock 被占用是最常見的情況。
pub fn describe_start_failure(message: &str) -> String {
    if message.contains("已由另一個 doujin-http instance 使用") {
        "此 catalog 已有另一個服務在使用（可能是舊版 私藏編目室.exe 啟動的背景服務）；\
         請先結束該服務再重新開啟"
            .to_owned()
    } else {
        message.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn stored_catalog_uses_the_launcher_json_format() {
        let directory = test_directory("store");
        fs::create_dir_all(&directory).expect("directory");

        store_catalog(&directory, Path::new("D:\\catalog.db")).expect("store catalog");

        assert_eq!(
            "{\n  \"catalog\": \"D:\\\\catalog.db\"\n}",
            fs::read_to_string(directory.join("launcher.json")).expect("written config")
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn catalog_written_by_the_launcher_is_read_back() {
        let directory = test_directory("read");
        fs::create_dir_all(&directory).expect("directory");
        fs::write(
            directory.join("launcher.json"),
            "{\n  \"catalog\": \"D:\\\\library\\\\catalog.db\"\n}",
        )
        .expect("launcher config");

        assert_eq!(
            Some(PathBuf::from("D:\\library\\catalog.db")),
            read_catalog(&directory)
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn missing_or_invalid_config_means_first_launch() {
        let directory = test_directory("first-launch");
        fs::create_dir_all(&directory).expect("directory");
        assert_eq!(None, read_catalog(&directory));

        fs::write(directory.join("launcher.json"), "{ not json").expect("invalid config");
        assert_eq!(None, read_catalog(&directory));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn onboarding_sends_the_window_to_the_settings_view() {
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 51234);
        assert_eq!("http://127.0.0.1:51234/", window_url(address, false));
        assert_eq!(
            "http://127.0.0.1:51234/#settings",
            window_url(address, true)
        );
    }

    #[test]
    fn a_busy_service_lock_names_the_background_service() {
        let described =
            describe_start_failure("此 catalog 已由另一個 doujin-http instance 使用 (os error 33)");
        assert!(described.contains("舊版 私藏編目室.exe"));
        assert_eq!(
            "catalog 不是一般檔案",
            describe_start_failure("catalog 不是一般檔案")
        );
    }

    fn test_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "doujin-desktop-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }
}
