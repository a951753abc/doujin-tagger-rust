use doujin_launcher::{Launcher, LauncherCommand, parse_arguments, usage};

fn main() {
    let command = match parse_arguments(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => fail(&format!("{error}\n\n{}", usage()), 64),
    };
    let launcher = Launcher::discover().unwrap_or_else(|error| {
        fail(&format!("無法準備 launcher：{error}"), 1);
    });
    match launcher.execute(command.clone()) {
        Ok(message) => {
            println!("{message}");
            #[cfg(windows)]
            if matches!(
                command,
                LauncherCommand::Status | LauncherCommand::Restart | LauncherCommand::Stop
            ) {
                show_windows_message(&message, "Information");
            }
        }
        Err(error) => fail(&format_launcher_error(&command, error.as_ref()), 1),
    }
}

fn format_launcher_error(command: &LauncherCommand, error: &dyn std::error::Error) -> String {
    let action = match command {
        LauncherCommand::Open { .. } => "無法開啟編目室",
        LauncherCommand::Status => "無法讀取服務狀態",
        LauncherCommand::Restart => "無法重新啟動服務",
        LauncherCommand::Stop => "無法停止服務",
        LauncherCommand::Help => "無法顯示說明",
    };
    format!("{action}：{error}")
}

fn fail(message: &str, exit_code: i32) -> ! {
    eprintln!("{message}");
    #[cfg(windows)]
    show_windows_error(message);
    std::process::exit(exit_code);
}

#[cfg(windows)]
fn show_windows_error(message: &str) {
    show_windows_message(message, "Error");
}

#[cfg(windows)]
fn show_windows_message(message: &str, icon: &str) {
    use std::process::{Command, Stdio};

    let escaped = message.replace('`', "``").replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName PresentationFramework; [System.Windows.MessageBox]::Show('{escaped}', 'JP6 Doujin Archive', 'OK', '{icon}') | Out-Null"
    );
    let _ = Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
