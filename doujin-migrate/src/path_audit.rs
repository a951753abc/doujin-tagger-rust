//! Read-only verification of current v2 collection paths before cutover.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use doujin_storage::path_key;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

const SAMPLE_LIMIT: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathAuditReport {
    pub catalog_path: String,
    pub catalog_open_mode: String,
    pub catalog_blake3: String,
    pub passed: bool,
    pub roots: Vec<RootAudit>,
    pub totals: PathAuditCounts,
    pub issues: BTreeMap<String, Vec<PathIssue>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootAudit {
    pub id: i64,
    pub path: String,
    pub source: String,
    pub label: String,
    pub active: bool,
    pub exists: bool,
    pub is_directory: bool,
    pub readable: bool,
    pub expected_current_paths: usize,
    pub counts: PathAuditCounts,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PathAuditCounts {
    pub current_paths: usize,
    pub valid_current_paths: usize,
    pub existing_regular_zip: usize,
    pub existing_image_folder: usize,
    pub missing: usize,
    pub inaccessible: usize,
    pub media_kind_mismatch: usize,
    pub symlink: usize,
    pub outside_registered_root: usize,
    pub rootless: usize,
}

impl PathAuditCounts {
    fn merge(&mut self, other: &Self) {
        self.current_paths += other.current_paths;
        self.valid_current_paths += other.valid_current_paths;
        self.existing_regular_zip += other.existing_regular_zip;
        self.existing_image_folder += other.existing_image_folder;
        self.missing += other.missing;
        self.inaccessible += other.inaccessible;
        self.media_kind_mismatch += other.media_kind_mismatch;
        self.symlink += other.symlink;
        self.outside_registered_root += other.outside_registered_root;
        self.rootless += other.rootless;
    }

    fn passed(&self) -> bool {
        self.current_paths == self.valid_current_paths
            && self.missing == 0
            && self.inaccessible == 0
            && self.media_kind_mismatch == 0
            && self.symlink == 0
            && self.outside_registered_root == 0
            && self.rootless == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathIssue {
    pub collection_id: i64,
    pub root_id: Option<i64>,
    pub path: String,
    pub detail: String,
}

#[derive(Debug)]
struct CurrentPath {
    collection_id: i64,
    root_id: Option<i64>,
    path: String,
    media_kind: String,
}

pub fn audit_v2_paths(catalog: impl AsRef<Path>) -> Result<PathAuditReport, String> {
    let catalog = absolute_existing_file(catalog.as_ref())?;
    reject_catalog_sidecars(&catalog)?;
    let fingerprint = blake3_file(&catalog)?;
    let uri = immutable_sqlite_uri(&catalog)?;
    let connection = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(|error| error.to_string())?;

    let mut roots = load_roots(&connection)?;
    let paths = load_current_paths(&connection)?;
    let mut issues = BTreeMap::<String, Vec<PathIssue>>::new();
    let mut unassigned_counts = PathAuditCounts::default();
    let root_indexes = roots
        .iter()
        .enumerate()
        .map(|(index, root)| (root.id, index))
        .collect::<BTreeMap<_, _>>();

    for current in paths {
        let Some(root_id) = current.root_id else {
            audit_path_metadata(&current, None, &mut unassigned_counts, &mut issues);
            continue;
        };
        let Some(index) = root_indexes.get(&root_id).copied() else {
            audit_path_metadata(&current, None, &mut unassigned_counts, &mut issues);
            push_issue(
                &mut issues,
                "unknown_root",
                &current,
                format!("catalog 指向不存在的 root #{root_id}"),
            );
            continue;
        };
        roots[index].expected_current_paths += 1;
        let root_path = PathBuf::from(&roots[index].path);
        audit_path_metadata(
            &current,
            Some(&root_path),
            &mut roots[index].counts,
            &mut issues,
        );
    }

    let mut totals = unassigned_counts;
    for root in &roots {
        totals.merge(&root.counts);
    }
    let roots_passed = roots
        .iter()
        .all(|root| root.active && root.exists && root.is_directory && root.readable);
    drop(connection);
    let fingerprint_after = blake3_file(&catalog)?;
    if fingerprint != fingerprint_after {
        return Err("path audit 前後 catalog BLAKE3 不一致".to_owned());
    }

    Ok(PathAuditReport {
        catalog_path: catalog.to_string_lossy().into_owned(),
        catalog_open_mode:
            "SQLite URI mode=ro&immutable=1 + SQLITE_OPEN_READ_ONLY + PRAGMA query_only=ON"
                .to_owned(),
        catalog_blake3: fingerprint,
        passed: roots_passed && totals.passed() && !issues.contains_key("unknown_root"),
        roots,
        totals,
        issues,
    })
}

fn load_roots(connection: &Connection) -> Result<Vec<RootAudit>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, path, source_kind, label, active
             FROM library_roots ORDER BY id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    rows.map(|row| {
        let (id, path, source, label, active) = row.map_err(|error| error.to_string())?;
        let root_path = Path::new(&path);
        let metadata = fs::metadata(root_path);
        let (exists, is_directory, readable, error) = match metadata {
            Ok(metadata) if !metadata.is_dir() => {
                (true, false, false, Some("root 不是資料夾".to_owned()))
            }
            Ok(_) => match fs::read_dir(root_path) {
                Ok(mut entries) => match entries.next() {
                    Some(Err(error)) => (true, true, false, Some(error.to_string())),
                    _ => (true, true, true, None),
                },
                Err(error) => (true, true, false, Some(error.to_string())),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (false, false, false, Some("root 不存在".to_owned()))
            }
            Err(error) => (false, false, false, Some(error.to_string())),
        };
        Ok(RootAudit {
            id,
            path,
            source,
            label,
            active,
            exists,
            is_directory,
            readable,
            expected_current_paths: 0,
            counts: PathAuditCounts::default(),
            error,
        })
    })
    .collect()
}

fn load_current_paths(connection: &Connection) -> Result<Vec<CurrentPath>, String> {
    let mut statement = connection
        .prepare(
            "SELECT location.collection_id, location.root_id, location.full_path,
                    collection.media_kind
             FROM collection_locations AS location
             JOIN collections AS collection ON collection.id = location.collection_id
             WHERE location.location_status = 'current' AND collection.status = 'active'
             ORDER BY location.collection_id",
        )
        .map_err(|error| error.to_string())?;
    statement
        .query_map([], |row| {
            Ok(CurrentPath {
                collection_id: row.get(0)?,
                root_id: row.get(1)?,
                path: row.get(2)?,
                media_kind: row.get(3)?,
            })
        })
        .map_err(|error| error.to_string())?
        .map(|row| row.map_err(|error| error.to_string()))
        .collect()
}

fn audit_path_metadata(
    current: &CurrentPath,
    root: Option<&Path>,
    counts: &mut PathAuditCounts,
    issues: &mut BTreeMap<String, Vec<PathIssue>>,
) {
    counts.current_paths += 1;
    let path = Path::new(&current.path);
    let within_root = root.is_some_and(|root| is_within_root(path, root));
    if root.is_none() {
        counts.rootless += 1;
        push_issue(
            issues,
            "rootless",
            current,
            "current path 沒有有效 root".to_owned(),
        );
    } else if !within_root {
        counts.outside_registered_root += 1;
        push_issue(
            issues,
            "outside_registered_root",
            current,
            "current path 不在註冊 root 內".to_owned(),
        );
    }

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            counts.missing += 1;
            push_issue(issues, "missing", current, "檔案不存在".to_owned());
            return;
        }
        Err(error) => {
            counts.inaccessible += 1;
            push_issue(issues, "inaccessible", current, error.to_string());
            return;
        }
    };
    if metadata.file_type().is_symlink() {
        counts.symlink += 1;
        push_issue(
            issues,
            "symlink",
            current,
            "current path 是 symlink".to_owned(),
        );
        return;
    }
    let is_zip = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
    let valid_kind = match current.media_kind.as_str() {
        "zip" if metadata.is_file() && is_zip => {
            if within_root {
                counts.existing_regular_zip += 1;
            }
            true
        }
        "image_folder" if metadata.is_dir() => {
            if within_root {
                counts.existing_image_folder += 1;
            }
            true
        }
        "zip" => {
            counts.media_kind_mismatch += 1;
            push_issue(
                issues,
                "media_kind_mismatch",
                current,
                "media_kind=zip，但 current path 不是 .zip 一般檔案".to_owned(),
            );
            false
        }
        "image_folder" => {
            counts.media_kind_mismatch += 1;
            push_issue(
                issues,
                "media_kind_mismatch",
                current,
                "media_kind=image_folder，但 current path 不是資料夾".to_owned(),
            );
            false
        }
        value => {
            counts.media_kind_mismatch += 1;
            push_issue(
                issues,
                "media_kind_mismatch",
                current,
                format!("未知 media_kind={value}"),
            );
            false
        }
    };
    if valid_kind && within_root {
        counts.valid_current_paths += 1;
    }
}

fn push_issue(
    issues: &mut BTreeMap<String, Vec<PathIssue>>,
    kind: &str,
    current: &CurrentPath,
    detail: String,
) {
    let samples = issues.entry(kind.to_owned()).or_default();
    if samples.len() < SAMPLE_LIMIT {
        samples.push(PathIssue {
            collection_id: current.collection_id,
            root_id: current.root_id,
            path: current.path.clone(),
            detail,
        });
    }
}

fn is_within_root(path: &Path, root: &Path) -> bool {
    let path = path_key(path);
    let root = path_key(root);
    path.starts_with(&format!("{root}\\"))
}

fn absolute_existing_file(path: &Path) -> Result<PathBuf, String> {
    if !path.is_file() {
        return Err(format!("找不到 v2 catalog：{}", path.display()));
    }
    fs::canonicalize(path).map_err(|error| error.to_string())
}

fn reject_catalog_sidecars(catalog: &Path) -> Result<(), String> {
    let existing = ["-wal", "-shm"]
        .into_iter()
        .map(|suffix| appended_path(catalog, suffix))
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    if existing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "v2 catalog 具有 sidecar，不是靜止副本：{}",
            existing
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("、")
        ))
    }
}

fn blake3_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn immutable_sqlite_uri(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or_else(|| format!("catalog path 不是有效 Unicode：{}", path.display()))?;
    let portable = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
    };
    let normalized = portable.replace('\\', "/");
    let mut encoded = String::with_capacity(normalized.len());
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    let uri = if encoded.starts_with("//") {
        format!("file:{encoded}")
    } else if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    };
    Ok(format!("{uri}?mode=ro&immutable=1"))
}

fn appended_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use doujin_storage::CatalogRepository;
    use rusqlite::params;

    use super::*;

    #[test]
    fn audit_reports_valid_missing_and_non_zip_paths_without_writing_catalog() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let base =
            std::env::temp_dir().join(format!("doujin-path-audit-{}-{unique}", std::process::id()));
        let root = base.join("library");
        fs::create_dir_all(&root).expect("root");
        let valid = root.join("valid.zip");
        let text = root.join("not-zip.txt");
        let image_folder = root.join("image-folder");
        fs::write(&valid, b"zip fixture").expect("valid file");
        fs::write(&text, b"text fixture").expect("text file");
        fs::create_dir(&image_folder).expect("image folder");
        let catalog = base.join("catalog.db");
        drop(CatalogRepository::open(&catalog).expect("catalog"));
        let connection = Connection::open(&catalog).expect("write fixture");
        connection
            .execute(
                "INSERT INTO library_roots(id, path, path_key, source_kind, label)
                 VALUES (1, ?1, ?2, 'archive', 'Test')",
                params![root.to_string_lossy(), path_key(&root)],
            )
            .expect("root row");
        for (id, path, media_kind) in [
            (1, valid.clone(), "zip"),
            (2, root.join("missing.zip"), "zip"),
            (3, text.clone(), "zip"),
            (4, image_folder.clone(), "image_folder"),
        ] {
            connection
                .execute(
                    "INSERT INTO collections(id, status, media_kind) VALUES (?1, 'active', ?2)",
                    params![id, media_kind],
                )
                .expect("collection");
            let filename = path.file_name().expect("filename").to_string_lossy();
            connection
                .execute(
                    "INSERT INTO collection_locations(
                         collection_id, root_id, full_path, path_key, relative_path,
                         filename, location_status
                     ) VALUES (?1, 1, ?2, ?3, ?4, ?4, 'current')",
                    params![id, path.to_string_lossy(), path_key(&path), filename],
                )
                .expect("location");
        }
        drop(connection);

        let before = fs::read(&catalog).expect("before");
        let report = audit_v2_paths(&catalog).expect("audit");
        assert!(!report.passed);
        assert_eq!(4, report.totals.current_paths);
        assert_eq!(2, report.totals.valid_current_paths);
        assert_eq!(1, report.totals.existing_regular_zip);
        assert_eq!(1, report.totals.existing_image_folder);
        assert_eq!(1, report.totals.missing);
        assert_eq!(1, report.totals.media_kind_mismatch);
        assert_eq!(before, fs::read(&catalog).expect("after"));
        assert!(!appended_path(&catalog, "-wal").exists());
        assert!(!appended_path(&catalog, "-shm").exists());

        fs::remove_dir_all(&base).expect("cleanup");
    }
}
