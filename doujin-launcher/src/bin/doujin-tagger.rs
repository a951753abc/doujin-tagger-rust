#![cfg_attr(windows, windows_subsystem = "windows")]

use doujin_launcher::{Launcher, LauncherCommand};

fn main() {
    let result = Launcher::discover()
        .and_then(|launcher| launcher.execute(LauncherCommand::Open { catalog: None }));
    if let Err(error) = result {
        show_error(&format!("無法開啟私藏編目室：{error}"));
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn show_error(message: &str) {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let escaped = message.replace('`', "``").replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName PresentationFramework; [System.Windows.MessageBox]::Show('{escaped}', '私藏編目室', 'OK', 'Error') | Out-Null"
    );
    let _ = Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn show_error(message: &str) {
    eprintln!("{message}");
}
