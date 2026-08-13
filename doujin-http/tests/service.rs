use std::fs;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use doujin_http::{ServiceOptions, run_service};
use doujin_storage::CatalogRepository;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

async fn get(address: SocketAddr, path: &str) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect in-process service");
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write HTTP request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read HTTP response");
    let response = String::from_utf8(response).expect("UTF-8 response");
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .expect("status line");
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .expect("response body");
    (status, body)
}

#[tokio::test]
async fn in_process_service_serves_health_until_shutdown_returns() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "doujin-http-in-process-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("create test tree");
    let database = directory.join("catalog.db");
    drop(CatalogRepository::open(&database).expect("create catalog"));

    let (ready_sender, ready_receiver) = oneshot::channel();
    let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
    let service = tokio::spawn(run_service(
        ServiceOptions {
            database: database.clone(),
            port: 0,
            config_path: directory.join("config.json"),
            instance: None,
        },
        move |address| {
            let _ = ready_sender.send(address);
        },
        async move {
            let _ = shutdown_receiver.await;
        },
    ));

    let address = ready_receiver.await.expect("bound address");
    assert!(address.ip().is_loopback());
    assert_ne!(0, address.port());

    let (status, body) = get(address, "/api/health").await;
    assert_eq!(200, status);
    assert!(body.contains("\"status\":\"ok\""), "health body: {body}");
    assert!(
        body.contains("\"service\":\"doujin-http\""),
        "health body: {body}"
    );

    shutdown_sender.send(()).expect("send shutdown");
    let outcome = tokio::time::timeout(Duration::from_secs(30), service)
        .await
        .expect("service returns after shutdown")
        .expect("service task");
    assert!(outcome.is_ok(), "service result: {outcome:?}");

    let _ = fs::remove_dir_all(&directory);
}
