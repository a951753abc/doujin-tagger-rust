#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;

#[test]
fn windows_binary_exposes_lifecycle_help_without_starting_a_service() {
    let output = Command::new(env!("CARGO_BIN_EXE_doujin-launcher"))
        .arg("--help")
        .output()
        .expect("run launcher help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    assert!(stdout.contains("open"));
    assert!(stdout.contains("status"));
    assert!(stdout.contains("restart"));
    assert!(stdout.contains("stop"));
}

#[test]
fn windows_install_script_is_valid_powershell() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tools")
        .join("install_windows_launcher.ps1");
    let escaped = script.to_string_lossy().replace('\'', "''");
    let command =
        format!("$null = [scriptblock]::Create([System.IO.File]::ReadAllText('{escaped}'))");
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &command])
        .status()
        .expect("parse install script");
    assert!(status.success());
}
