use std::path::PathBuf;

use doujin_migrate::{MigrationStatus, run_migration};

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let Some(source) = arguments.next() else {
        usage_and_exit();
    };
    let Some(target) = arguments.next() else {
        usage_and_exit();
    };
    if arguments.next().is_some() {
        usage_and_exit();
    }

    match run_migration(PathBuf::from(source), PathBuf::from(target)) {
        Ok(report) => {
            match serde_json::to_string_pretty(&report) {
                Ok(json) => println!("{json}"),
                Err(error) => {
                    eprintln!("無法輸出 migration report：{error}");
                    std::process::exit(1);
                }
            }
            match report.status {
                MigrationStatus::Completed => {}
                MigrationStatus::Blocked => std::process::exit(2),
                MigrationStatus::ValidationFailed => std::process::exit(3),
            }
        }
        Err(error) => {
            eprintln!("migration rehearsal 失敗：{error}");
            std::process::exit(1);
        }
    }
}

fn usage_and_exit() -> ! {
    eprintln!("用法：doujin-migrate <legacy-source.db> <new-v2-target.db>");
    std::process::exit(64);
}
