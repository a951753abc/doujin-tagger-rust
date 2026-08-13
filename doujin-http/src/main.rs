use std::path::PathBuf;

use doujin_http::{ServiceInstanceConfig, ServiceOptions, run_service};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("doujin-http 啟動失敗：{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

    let options = ServiceOptions {
        database: absolute_path(PathBuf::from(database))?,
        port,
        config_path: config_path()?,
        instance: ServiceInstanceConfig::from_environment()?,
    };
    run_service(
        options,
        |local_address| println!("doujin-http listening on http://{local_address}"),
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
    )
    .await?;
    Ok(())
}

fn absolute_path(path: PathBuf) -> Result<PathBuf, std::io::Error> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn config_path() -> Result<PathBuf, std::io::Error> {
    match std::env::var_os("DOUJIN_CONFIG_PATH").filter(|value| !value.is_empty()) {
        Some(path) => absolute_path(PathBuf::from(path)),
        None => absolute_path(PathBuf::from("config.json")),
    }
}

fn usage_and_exit() -> ! {
    eprintln!("用法：doujin-http <v2-catalog.db> [port]");
    std::process::exit(64);
}
