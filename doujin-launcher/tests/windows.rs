#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
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
fn desktop_binary_uses_windows_gui_subsystem() {
    let executable = Path::new(env!("CARGO_BIN_EXE_doujin-tagger"));
    let bytes = fs::read(executable).expect("read desktop executable");
    let pe_offset =
        u32::from_le_bytes(bytes[0x3c..0x40].try_into().expect("DOS header PE offset")) as usize;
    assert_eq!(b"PE\0\0", &bytes[pe_offset..pe_offset + 4]);

    let optional_header = pe_offset + 4 + 20;
    let subsystem = u16::from_le_bytes(
        bytes[optional_header + 68..optional_header + 70]
            .try_into()
            .expect("PE subsystem"),
    );
    assert_eq!(2, subsystem, "desktop entry must not open a console window");
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

    let contents = fs::read_to_string(script).expect("read install script");
    assert!(contents.contains("doujin-tagger.exe"));
    assert!(contents.contains("JP6 Doujin Archive.exe"));
}
