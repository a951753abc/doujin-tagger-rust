use std::fs::{self, File, OpenOptions};
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::Serialize;

const METADATA_ENV: &str = "DOUJIN_INSTANCE_METADATA";
const LOCK_ENV: &str = "DOUJIN_INSTANCE_LOCK";
const ID_ENV: &str = "DOUJIN_INSTANCE_ID";

#[derive(Debug, Serialize)]
struct InstanceMetadata<'a> {
    schema: u8,
    pid: u32,
    catalog: &'a Path,
    port: u16,
    instance_id: &'a str,
    started_at_unix: u64,
}

pub(crate) struct ServiceInstanceGuard {
    _lock: File,
    metadata_path: PathBuf,
    instance_id: String,
}

impl ServiceInstanceGuard {
    pub(crate) fn from_environment() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let metadata_path = std::env::var_os(METADATA_ENV).map(PathBuf::from);
        let lock_path = std::env::var_os(LOCK_ENV).map(PathBuf::from);
        let instance_id = std::env::var(ID_ENV).ok().filter(|value| !value.is_empty());
        match (metadata_path, lock_path, instance_id) {
            (None, None, None) => Ok(None),
            (Some(metadata_path), Some(lock_path), Some(instance_id)) => {
                Self::acquire(metadata_path, lock_path, instance_id).map(Some)
            }
            _ => {
                Err("launcher instance 設定不完整；請同時提供 metadata、lock 與 instance ID".into())
            }
        }
    }

    fn acquire(
        metadata_path: PathBuf,
        lock_path: PathBuf,
        instance_id: String,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if let Some(parent) = metadata_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        lock.try_lock_exclusive().map_err(|error| {
            io::Error::new(
                error.kind(),
                "此 catalog 已由另一個 doujin-http instance 使用",
            )
        })?;
        Ok(Self {
            _lock: lock,
            metadata_path,
            instance_id,
        })
    }

    pub(crate) fn publish(&self, catalog: &Path, address: SocketAddr) -> io::Result<()> {
        let metadata = InstanceMetadata {
            schema: 1,
            pid: std::process::id(),
            catalog,
            port: address.port(),
            instance_id: &self.instance_id,
            started_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        let bytes = serde_json::to_vec_pretty(&metadata).map_err(io::Error::other)?;
        let temporary = self.metadata_path.with_extension("json.tmp");
        fs::write(&temporary, bytes)?;
        if self.metadata_path.exists() {
            fs::remove_file(&self.metadata_path)?;
        }
        fs::rename(temporary, &self.metadata_path)
    }
}

impl Drop for ServiceInstanceGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.metadata_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_contains_loopback_port_and_catalog() {
        let directory = std::env::temp_dir().join(format!(
            "doujin-http-instance-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("temp directory");
        let metadata_path = directory.join("instance.json");
        let lock_path = directory.join("service.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .expect("lock file");
        lock.try_lock_exclusive().expect("exclusive lock");
        let guard = ServiceInstanceGuard {
            _lock: lock,
            metadata_path: metadata_path.clone(),
            instance_id: "test-instance".to_owned(),
        };
        let catalog = directory.join("catalog.db");
        guard
            .publish(&catalog, "127.0.0.1:43123".parse().expect("address"))
            .expect("publish metadata");

        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&metadata_path).expect("metadata")).expect("json");
        assert_eq!(1, value["schema"]);
        assert_eq!(43123, value["port"]);
        assert_eq!(
            Some(catalog.to_string_lossy().as_ref()),
            value["catalog"].as_str()
        );
        assert_eq!("test-instance", value["instance_id"]);

        drop(guard);
        assert!(!metadata_path.exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn service_lock_rejects_a_competing_instance() {
        let directory = std::env::temp_dir().join(format!(
            "doujin-http-lock-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("temp directory");
        let lock_path = directory.join("service.lock");
        let first = ServiceInstanceGuard::acquire(
            directory.join("first.json"),
            lock_path.clone(),
            "first".to_owned(),
        )
        .expect("first service lock");
        let second = ServiceInstanceGuard::acquire(
            directory.join("second.json"),
            lock_path,
            "second".to_owned(),
        );
        assert!(second.is_err());
        drop(first);
        let _ = fs::remove_dir_all(directory);
    }
}
