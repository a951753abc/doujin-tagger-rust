use std::path::PathBuf;
use std::{fs, io::Write};

use doujin_migrate::path_audit::audit_v2_paths;

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let Some(catalog) = arguments.next() else {
        usage_and_exit();
    };
    let output = arguments.next().map(PathBuf::from);
    if arguments.next().is_some() {
        usage_and_exit();
    }

    match audit_v2_paths(PathBuf::from(catalog)) {
        Ok(report) => {
            match serde_json::to_string_pretty(&report) {
                Ok(json) => {
                    if let Some(output) = output {
                        let mut file = match fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&output)
                        {
                            Ok(file) => file,
                            Err(error) => {
                                eprintln!(
                                    "無法建立 path audit report {}：{error}",
                                    output.display()
                                );
                                std::process::exit(1);
                            }
                        };
                        if let Err(error) = writeln!(file, "{json}") {
                            eprintln!("無法寫入 path audit report {}：{error}", output.display());
                            std::process::exit(1);
                        }
                    } else {
                        println!("{json}");
                    }
                }
                Err(error) => {
                    eprintln!("無法輸出 path audit report：{error}");
                    std::process::exit(1);
                }
            }
            if !report.passed {
                std::process::exit(2);
            }
        }
        Err(error) => {
            eprintln!("path audit 失敗：{error}");
            std::process::exit(1);
        }
    }
}

fn usage_and_exit() -> ! {
    eprintln!("用法：doujin-path-audit <v2-catalog.db> [new-report.json]");
    std::process::exit(64);
}
