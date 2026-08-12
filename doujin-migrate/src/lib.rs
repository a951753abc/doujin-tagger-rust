//! Read-only migration rehearsal from the Python catalog to a new Rust v2 catalog.

pub mod path_audit;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use doujin_parser::domain::{Authors, Classification, Parody};
use doujin_storage::legacy::{
    LegacyCatalog, LegacyCollection, LegacyImportCounts, LegacyLibraryRoot, LegacyMediaKind,
    LegacyRootSource, LegacyTag, LegacyTagLink,
};
use doujin_storage::{CatalogRepository, StorageError, path_key};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const SAMPLE_LIMIT: usize = 100;

#[derive(Debug)]
pub enum MigrationError {
    Io(io::Error),
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Storage(StorageError),
    MissingSource(PathBuf),
    SourceHasSidecars(Vec<PathBuf>),
    InvalidSourceSchema(String),
    TargetAlreadyExists(Vec<PathBuf>),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O 錯誤：{error}"),
            Self::Sqlite(error) => write!(formatter, "SQLite 錯誤：{error}"),
            Self::Json(error) => write!(formatter, "JSON 錯誤：{error}"),
            Self::Storage(error) => write!(formatter, "v2 storage 錯誤：{error}"),
            Self::MissingSource(path) => {
                write!(formatter, "找不到 legacy catalog：{}", path.display())
            }
            Self::SourceHasSidecars(paths) => write!(
                formatter,
                "legacy source 旁存在 WAL/SHM，拒絕以 immutable 模式忽略未合併資料：{}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join("、")
            ),
            Self::InvalidSourceSchema(reason) => {
                write!(formatter, "legacy catalog schema 無效：{reason}")
            }
            Self::TargetAlreadyExists(paths) => write!(
                formatter,
                "拒絕覆寫既有 target artifact：{}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join("、")
            ),
        }
    }
}

impl Error for MigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::MissingSource(_)
            | Self::SourceHasSidecars(_)
            | Self::InvalidSourceSchema(_)
            | Self::TargetAlreadyExists(_) => None,
        }
    }
}

impl From<io::Error> for MigrationError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for MigrationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<serde_json::Error> for MigrationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<StorageError> for MigrationError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

pub type MigrationResult<T> = Result<T, MigrationError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStatus {
    Completed,
    Blocked,
    ValidationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogCounts {
    pub roots: usize,
    pub collections: usize,
    pub zip_collections: usize,
    pub image_folders: usize,
    pub locations: usize,
    pub assertions: usize,
    pub selections: usize,
    pub effective_metadata: usize,
    pub tags: usize,
    pub tag_links: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceCounts {
    pub roots: usize,
    pub collections: usize,
    pub zip_collections: usize,
    pub image_folders: usize,
    pub tags: usize,
    pub tag_links: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CountComparison {
    pub source: usize,
    pub target: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceFingerprint {
    pub before_blake3: String,
    pub after_blake3: String,
    pub unchanged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathConflict {
    pub normalized_path: String,
    pub legacy_ids: Vec<i64>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockingIssue {
    pub kind: String,
    pub legacy_id: Option<i64>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassificationMapping {
    pub legacy_value: String,
    pub top_level: String,
    pub subcategory: Option<String>,
    pub rows: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetadataMismatch {
    pub collection_id: i64,
    pub field: String,
    pub expected: Value,
    pub actual: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SampleValidation {
    pub requested: usize,
    pub checked: usize,
    pub mismatches: Vec<MetadataMismatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationSummary {
    pub integrity_check: Option<String>,
    pub foreign_key_violations: usize,
    pub count_mismatches: Vec<String>,
    pub tag_name_mismatches: usize,
    pub tag_link_mismatches: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MigrationReport {
    pub status: MigrationStatus,
    pub source_path: String,
    pub target_path: String,
    pub source_open_mode: String,
    pub source_fingerprint: SourceFingerprint,
    pub source_counts: SourceCounts,
    pub target_counts: Option<CatalogCounts>,
    pub source_empty_values: BTreeMap<String, usize>,
    pub effective_empty_value_comparison: BTreeMap<String, CountComparison>,
    pub path_conflicts: Vec<PathConflict>,
    pub blocking_issues: Vec<BlockingIssue>,
    pub classification_mappings: Vec<ClassificationMapping>,
    pub ignored_setting_keys: Vec<String>,
    pub reapply_setting_values: BTreeMap<String, String>,
    pub sample_metadata: SampleValidation,
    pub validation: ValidationSummary,
}

impl MigrationReport {
    pub fn passed(&self) -> bool {
        self.status == MigrationStatus::Completed
    }
}

#[derive(Debug)]
struct LegacyRow {
    id: i64,
    filename: Option<String>,
    filepath: Option<String>,
    folder: Option<String>,
    event: Option<String>,
    circle: Option<String>,
    author: Option<String>,
    title: Option<String>,
    parody: Option<String>,
    is_dl: i64,
    category: Option<String>,
    source: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RootSetting {
    path: String,
    source: String,
    label: String,
}

struct LoadedLegacy {
    rows: Vec<LegacyRow>,
    roots: Vec<RootSetting>,
    tags: Vec<LegacyTag>,
    tag_links: Vec<LegacyTagLink>,
    source_empty_values: BTreeMap<String, usize>,
    ignored_setting_keys: Vec<String>,
    reapply_setting_values: BTreeMap<String, String>,
    load_issues: Vec<BlockingIssue>,
}

struct PreparedLegacy {
    catalog: LegacyCatalog,
    path_conflicts: Vec<PathConflict>,
    blocking_issues: Vec<BlockingIssue>,
    classification_mappings: Vec<ClassificationMapping>,
}

/// Runs a rehearsal that can only create a previously nonexistent target.
/// The source connection is opened with SQLite READ_ONLY plus query_only.
pub fn run_migration(
    source: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> MigrationResult<MigrationReport> {
    let source = absolute_existing_file(source.as_ref())?;
    let target = absolute_target(target.as_ref())?;
    reject_source_sidecars(&source)?;
    reject_existing_target_artifacts(&target)?;

    let before_hash = blake3_file(&source)?;
    let loaded = load_legacy(&source)?;
    let zip_collections = loaded
        .rows
        .iter()
        .filter(|row| legacy_media_kind(row.filepath.as_deref()) == LegacyMediaKind::Zip)
        .count();
    let source_counts = SourceCounts {
        roots: loaded.roots.len(),
        collections: loaded.rows.len(),
        zip_collections,
        image_folders: loaded.rows.len() - zip_collections,
        tags: loaded.tags.len(),
        tag_links: loaded.tag_links.len(),
    };
    let prepared = prepare_legacy(&loaded);
    let mut report = base_report(
        &source,
        &target,
        before_hash,
        source_counts,
        &loaded,
        &prepared,
    );

    if !prepared.blocking_issues.is_empty() || !prepared.path_conflicts.is_empty() {
        finish_source_fingerprint(&source, &mut report)?;
        return Ok(report);
    }

    let import_result = (|| -> MigrationResult<LegacyImportCounts> {
        let mut repository = CatalogRepository::open(&target)?;
        Ok(repository.import_legacy_catalog(&prepared.catalog)?)
    })();
    let imported = match import_result {
        Ok(counts) => counts,
        Err(error) => {
            remove_new_target_artifacts(&target);
            return Err(error);
        }
    };

    validate_target(&target, &loaded, &prepared.catalog, imported, &mut report)?;
    finish_source_fingerprint(&source, &mut report)?;
    let validation_passed = report.source_fingerprint.unchanged
        && report.validation.integrity_check.as_deref() == Some("ok")
        && report.validation.foreign_key_violations == 0
        && report.validation.count_mismatches.is_empty()
        && report.validation.tag_name_mismatches == 0
        && report.validation.tag_link_mismatches == 0
        && report.sample_metadata.mismatches.is_empty();
    report.status = if validation_passed {
        MigrationStatus::Completed
    } else {
        MigrationStatus::ValidationFailed
    };
    Ok(report)
}

fn base_report(
    source: &Path,
    target: &Path,
    before_hash: String,
    source_counts: SourceCounts,
    loaded: &LoadedLegacy,
    prepared: &PreparedLegacy,
) -> MigrationReport {
    let effective_empty_value_comparison = [
        ("title", "title"),
        ("event", "event"),
        ("circle", "circle"),
        ("authors", "author"),
        ("parody", "parody"),
        ("classification", "category"),
        ("is_dl", "is_dl"),
    ]
    .into_iter()
    .map(|(target_field, source_field)| {
        (
            target_field.to_owned(),
            CountComparison {
                source: loaded
                    .source_empty_values
                    .get(source_field)
                    .copied()
                    .unwrap_or(0),
                target: None,
            },
        )
    })
    .collect();

    MigrationReport {
        status: MigrationStatus::Blocked,
        source_path: source.display().to_string(),
        target_path: target.display().to_string(),
        source_open_mode:
            "SQLite URI mode=ro&immutable=1 + SQLITE_OPEN_READ_ONLY + PRAGMA query_only=ON"
                .to_owned(),
        source_fingerprint: SourceFingerprint {
            before_blake3: before_hash,
            after_blake3: String::new(),
            unchanged: false,
        },
        source_counts,
        target_counts: None,
        source_empty_values: loaded.source_empty_values.clone(),
        effective_empty_value_comparison,
        path_conflicts: prepared.path_conflicts.clone(),
        blocking_issues: prepared.blocking_issues.clone(),
        classification_mappings: prepared.classification_mappings.clone(),
        ignored_setting_keys: loaded.ignored_setting_keys.clone(),
        reapply_setting_values: loaded.reapply_setting_values.clone(),
        sample_metadata: SampleValidation {
            requested: SAMPLE_LIMIT,
            checked: 0,
            mismatches: Vec::new(),
        },
        validation: ValidationSummary {
            integrity_check: None,
            foreign_key_violations: 0,
            count_mismatches: Vec::new(),
            tag_name_mismatches: 0,
            tag_link_mismatches: 0,
        },
    }
}

fn load_legacy(path: &Path) -> MigrationResult<LoadedLegacy> {
    let source_uri = immutable_sqlite_uri(path)?;
    let connection = Connection::open_with_flags(
        source_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.pragma_update(None, "query_only", true)?;
    validate_legacy_schema(&connection)?;

    let mut load_issues = Vec::new();
    let root_json = connection
        .query_row(
            "SELECT value FROM settings WHERE key = 'scan_roots'",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let roots = match root_json {
        Some(value) => match serde_json::from_str::<Vec<RootSetting>>(&value) {
            Ok(roots) => roots,
            Err(error) => {
                load_issues.push(BlockingIssue {
                    kind: "invalid_scan_roots_json".to_owned(),
                    legacy_id: None,
                    detail: error.to_string(),
                });
                Vec::new()
            }
        },
        None => {
            load_issues.push(BlockingIssue {
                kind: "missing_scan_roots".to_owned(),
                legacy_id: None,
                detail: "settings.scan_roots 不存在，無法安全建立 library roots".to_owned(),
            });
            Vec::new()
        }
    };

    let rows = {
        let mut statement = connection.prepare(
            "SELECT id, filename, filepath, folder, event, circle, author, title, parody,
                    is_dl, category, source, created_at, updated_at
             FROM doujinshi ORDER BY id",
        )?;
        statement
            .query_map([], |row| {
                Ok(LegacyRow {
                    id: row.get(0)?,
                    filename: row.get(1)?,
                    filepath: row.get(2)?,
                    folder: row.get(3)?,
                    event: row.get(4)?,
                    circle: row.get(5)?,
                    author: row.get(6)?,
                    title: row.get(7)?,
                    parody: row.get(8)?,
                    is_dl: row.get(9)?,
                    category: row.get(10)?,
                    source: row.get(11)?,
                    created_at: row.get(12)?,
                    updated_at: row.get(13)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let tags = {
        let mut statement = connection.prepare("SELECT id, name FROM tags ORDER BY id")?;
        statement
            .query_map([], |row| {
                Ok(LegacyTag {
                    id: row.get(0)?,
                    name: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let tag_links = {
        let mut statement = connection.prepare(
            "SELECT doujinshi_id, tag_id FROM doujinshi_tags ORDER BY doujinshi_id, tag_id",
        )?;
        statement
            .query_map([], |row| {
                Ok(LegacyTagLink {
                    collection_id: row.get(0)?,
                    tag_id: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let ignored_setting_keys = {
        let mut statement = connection
            .prepare("SELECT key FROM settings WHERE key <> 'scan_roots' ORDER BY key")?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    let reapply_setting_values = {
        let mut statement = connection.prepare(
            "SELECT key, value FROM settings
             WHERE key IN ('viewer_path', 'thumb_size', 'thumb_quality')
             ORDER BY key",
        )?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<BTreeMap<_, _>, _>>()?
    };
    let source_empty_values = source_empty_counts(&rows);

    Ok(LoadedLegacy {
        rows,
        roots,
        tags,
        tag_links,
        source_empty_values,
        ignored_setting_keys,
        reapply_setting_values,
        load_issues,
    })
}

fn validate_legacy_schema(connection: &Connection) -> MigrationResult<()> {
    for (table, required) in [
        (
            "doujinshi",
            &[
                "id",
                "filename",
                "filepath",
                "folder",
                "event",
                "circle",
                "author",
                "title",
                "parody",
                "is_dl",
                "category",
                "source",
                "created_at",
                "updated_at",
            ][..],
        ),
        ("tags", &["id", "name"][..]),
        ("doujinshi_tags", &["doujinshi_id", "tag_id"][..]),
        ("settings", &["key", "value"][..]),
    ] {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<HashSet<_>, _>>()?;
        let missing = required
            .iter()
            .filter(|column| !columns.contains(**column))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(MigrationError::InvalidSourceSchema(format!(
                "{table} 缺少欄位：{}",
                missing.join("、")
            )));
        }
    }
    Ok(())
}

fn prepare_legacy(loaded: &LoadedLegacy) -> PreparedLegacy {
    let mut issues = loaded.load_issues.clone();
    let mut root_keys = HashSet::new();
    let mut roots = Vec::with_capacity(loaded.roots.len());
    for root in &loaded.roots {
        let source = match parse_root_source(&root.source) {
            Some(source) => source,
            None => {
                issues.push(BlockingIssue {
                    kind: "invalid_root_source".to_owned(),
                    legacy_id: None,
                    detail: format!("root {} 的 source 為 {}", root.path, root.source),
                });
                continue;
            }
        };
        if root.path.trim().is_empty() || root.label.trim().is_empty() {
            issues.push(BlockingIssue {
                kind: "invalid_library_root".to_owned(),
                legacy_id: None,
                detail: format!("root path 或 label 為空白：{}", root.path),
            });
            continue;
        }
        let key = normalized_path(&root.path);
        if !root_keys.insert(key) {
            issues.push(BlockingIssue {
                kind: "duplicate_library_root".to_owned(),
                legacy_id: None,
                detail: root.path.clone(),
            });
            continue;
        }
        roots.push(LegacyLibraryRoot {
            path: PathBuf::from(&root.path),
            source,
            label: root.label.clone(),
        });
    }

    let path_conflicts = find_path_conflicts(&loaded.rows);
    let collection_ids = loaded.rows.iter().map(|row| row.id).collect::<HashSet<_>>();
    let tag_ids = loaded.tags.iter().map(|tag| tag.id).collect::<HashSet<_>>();
    validate_tags(loaded, &collection_ids, &tag_ids, &mut issues);

    let mut collections = Vec::with_capacity(loaded.rows.len());
    let mut classification_counts: BTreeMap<(String, String, Option<String>), usize> =
        BTreeMap::new();
    for row in &loaded.rows {
        let Some(filename) = required_text(&row.filename, "filename", row.id, &mut issues) else {
            continue;
        };
        let Some(filepath) = required_text(&row.filepath, "filepath", row.id, &mut issues) else {
            continue;
        };
        let Some(title) = required_text(&row.title, "title", row.id, &mut issues) else {
            continue;
        };
        if row.id <= 0 {
            issues.push(BlockingIssue {
                kind: "invalid_collection_id".to_owned(),
                legacy_id: Some(row.id),
                detail: row.id.to_string(),
            });
            continue;
        }
        if ![0, 1].contains(&row.is_dl) {
            issues.push(BlockingIssue {
                kind: "invalid_is_dl".to_owned(),
                legacy_id: Some(row.id),
                detail: row.is_dl.to_string(),
            });
            continue;
        }
        let path_filename = filename_from_path(&filepath);
        if path_filename != filename {
            issues.push(BlockingIssue {
                kind: "filename_path_mismatch".to_owned(),
                legacy_id: Some(row.id),
                detail: format!("filename={filename}，filepath 尾段={path_filename}"),
            });
            continue;
        }
        let Some((root, relative_path)) = match_root(&filepath, &loaded.roots) else {
            issues.push(BlockingIssue {
                kind: "path_outside_scan_roots".to_owned(),
                legacy_id: Some(row.id),
                detail: filepath,
            });
            continue;
        };
        let Some(row_source) = row.source.as_deref().and_then(parse_root_source) else {
            issues.push(BlockingIssue {
                kind: "invalid_collection_source".to_owned(),
                legacy_id: Some(row.id),
                detail: row.source.clone().unwrap_or_default(),
            });
            continue;
        };
        let Some(root_source) = parse_root_source(&root.source) else {
            continue;
        };
        if row_source != root_source {
            issues.push(BlockingIssue {
                kind: "root_source_mismatch".to_owned(),
                legacy_id: Some(row.id),
                detail: format!("collection={row_source:?}，root={root_source:?}"),
            });
            continue;
        }

        let legacy_category = nonempty_text(row.category.as_deref()).unwrap_or("其他");
        let classification = legacy_classification(legacy_category);
        *classification_counts
            .entry((
                legacy_category.to_owned(),
                classification.top_level.clone(),
                classification.subcategory.clone(),
            ))
            .or_default() += 1;
        let author = nonempty_owned(row.author.as_deref());
        collections.push(LegacyCollection {
            id: row.id,
            filename,
            filepath: PathBuf::from(&filepath),
            media_kind: legacy_media_kind(Some(&filepath)),
            root_path: PathBuf::from(&root.path),
            relative_path,
            event: nonempty_owned(row.event.as_deref()),
            circle: nonempty_owned(row.circle.as_deref()),
            authors: Authors {
                raw: author.clone(),
                values: split_authors(author.as_deref()),
            },
            title,
            parody: nonempty_owned(row.parody.as_deref()).map(|value| Parody {
                raw: value.clone(),
                canonical: value,
                evidence: "legacy_catalog_current_value".to_owned(),
            }),
            classification,
            is_dl: row.is_dl == 1,
            created_at: nonempty_owned(row.created_at.as_deref()),
            updated_at: nonempty_owned(row.updated_at.as_deref()),
        });
    }

    let classification_mappings = classification_counts
        .into_iter()
        .map(
            |((legacy_value, top_level, subcategory), rows)| ClassificationMapping {
                legacy_value,
                top_level,
                subcategory,
                rows,
            },
        )
        .collect();
    PreparedLegacy {
        catalog: LegacyCatalog {
            roots,
            collections,
            tags: loaded.tags.clone(),
            tag_links: loaded.tag_links.clone(),
        },
        path_conflicts,
        blocking_issues: issues,
        classification_mappings,
    }
}

fn validate_tags(
    loaded: &LoadedLegacy,
    collection_ids: &HashSet<i64>,
    tag_ids: &HashSet<i64>,
    issues: &mut Vec<BlockingIssue>,
) {
    let mut names = HashSet::new();
    for tag in &loaded.tags {
        if tag.id <= 0 || tag.name.trim().is_empty() {
            issues.push(BlockingIssue {
                kind: "invalid_tag".to_owned(),
                legacy_id: Some(tag.id),
                detail: tag.name.clone(),
            });
        }
        if !names.insert(tag.name.clone()) {
            issues.push(BlockingIssue {
                kind: "duplicate_tag_name".to_owned(),
                legacy_id: Some(tag.id),
                detail: tag.name.clone(),
            });
        }
    }
    for link in &loaded.tag_links {
        if !collection_ids.contains(&link.collection_id) || !tag_ids.contains(&link.tag_id) {
            issues.push(BlockingIssue {
                kind: "orphan_tag_link".to_owned(),
                legacy_id: Some(link.collection_id),
                detail: format!("tag_id={}", link.tag_id),
            });
        }
    }
}

fn validate_target(
    target: &Path,
    loaded: &LoadedLegacy,
    catalog: &LegacyCatalog,
    imported: LegacyImportCounts,
    report: &mut MigrationReport,
) -> MigrationResult<()> {
    let connection = Connection::open_with_flags(
        target,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.pragma_update(None, "query_only", true)?;
    let actual = target_counts(&connection)?;
    report.target_counts = Some(actual.clone());
    let expected = CatalogCounts {
        roots: imported.roots,
        collections: imported.collections,
        zip_collections: catalog
            .collections
            .iter()
            .filter(|collection| collection.media_kind == LegacyMediaKind::Zip)
            .count(),
        image_folders: catalog
            .collections
            .iter()
            .filter(|collection| collection.media_kind == LegacyMediaKind::ImageFolder)
            .count(),
        locations: imported.locations,
        assertions: imported.assertions,
        selections: imported.selections,
        effective_metadata: imported.effective_metadata,
        tags: imported.tags,
        tag_links: imported.tag_links,
    };
    for (field, expected, actual) in [
        ("roots", expected.roots, actual.roots),
        ("collections", expected.collections, actual.collections),
        (
            "zip_collections",
            expected.zip_collections,
            actual.zip_collections,
        ),
        (
            "image_folders",
            expected.image_folders,
            actual.image_folders,
        ),
        ("locations", expected.locations, actual.locations),
        ("assertions", expected.assertions, actual.assertions),
        ("selections", expected.selections, actual.selections),
        (
            "effective_metadata",
            expected.effective_metadata,
            actual.effective_metadata,
        ),
        ("tags", expected.tags, actual.tags),
        ("tag_links", expected.tag_links, actual.tag_links),
    ] {
        if expected != actual {
            report
                .validation
                .count_mismatches
                .push(format!("{field}: expected={expected}, actual={actual}"));
        }
    }

    let target_empty = target_empty_counts(&connection)?;
    for (field, comparison) in &mut report.effective_empty_value_comparison {
        comparison.target = target_empty.get(field).copied();
    }
    report.validation.integrity_check =
        Some(connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?);
    report.validation.foreign_key_violations = {
        let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
        statement.query_map([], |_| Ok(()))?.count()
    };

    let target_tags = read_target_tags(&connection)?;
    let source_tags = loaded
        .tags
        .iter()
        .map(|tag| (tag.id, tag.name.clone()))
        .collect::<BTreeMap<_, _>>();
    report.validation.tag_name_mismatches = symmetric_map_mismatches(&source_tags, &target_tags);
    let target_links = read_target_tag_links(&connection)?;
    let source_links = loaded
        .tag_links
        .iter()
        .map(|link| (link.collection_id, link.tag_id))
        .collect::<BTreeSet<_>>();
    report.validation.tag_link_mismatches =
        source_links.symmetric_difference(&target_links).count();

    validate_samples(&connection, catalog, &mut report.sample_metadata)?;
    Ok(())
}

fn target_counts(connection: &Connection) -> MigrationResult<CatalogCounts> {
    Ok(CatalogCounts {
        roots: scalar_count(connection, "library_roots")?,
        collections: scalar_count(connection, "collections")?,
        zip_collections: scalar_where_count(connection, "collections", "media_kind = 'zip'")?,
        image_folders: scalar_where_count(
            connection,
            "collections",
            "media_kind = 'image_folder'",
        )?,
        locations: scalar_count(connection, "collection_locations")?,
        assertions: scalar_count(connection, "metadata_assertions")?,
        selections: scalar_count(connection, "metadata_selections")?,
        effective_metadata: scalar_count(connection, "effective_metadata")?,
        tags: scalar_count(connection, "tags")?,
        tag_links: scalar_count(connection, "collection_tags")?,
    })
}

fn scalar_where_count(
    connection: &Connection,
    table: &str,
    predicate: &str,
) -> MigrationResult<usize> {
    let count: i64 = connection.query_row(
        &format!("SELECT count(*) FROM {table} WHERE {predicate}"),
        [],
        |row| row.get(0),
    )?;
    usize::try_from(count)
        .map_err(|_| MigrationError::InvalidSourceSchema(format!("{table} count 無效")))
}

fn scalar_count(connection: &Connection, table: &str) -> MigrationResult<usize> {
    let count: i64 = connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })?;
    usize::try_from(count)
        .map_err(|_| MigrationError::InvalidSourceSchema(format!("{table} count 無效")))
}

fn target_empty_counts(connection: &Connection) -> MigrationResult<BTreeMap<String, usize>> {
    let row = connection.query_row(
        "SELECT
             coalesce(sum(CASE WHEN title IS NULL OR trim(title) = '' THEN 1 ELSE 0 END), 0),
             coalesce(sum(CASE WHEN event IS NULL OR trim(event) = '' THEN 1 ELSE 0 END), 0),
             coalesce(sum(CASE WHEN circle IS NULL OR trim(circle) = '' THEN 1 ELSE 0 END), 0),
             coalesce(sum(CASE WHEN trim(authors) = '' THEN 1 ELSE 0 END), 0),
             coalesce(sum(CASE WHEN parody IS NULL OR trim(parody) = '' THEN 1 ELSE 0 END), 0),
             coalesce(sum(CASE WHEN classification_top IS NULL OR trim(classification_top) = ''
                               THEN 1 ELSE 0 END), 0),
             coalesce(sum(CASE WHEN is_dl IS NULL THEN 1 ELSE 0 END), 0)
         FROM effective_metadata",
        [],
        |row| {
            Ok([
                row.get::<_, i64>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ])
        },
    )?;
    Ok([
        "title",
        "event",
        "circle",
        "authors",
        "parody",
        "classification",
        "is_dl",
    ]
    .into_iter()
    .zip(row)
    .map(|(field, count)| (field.to_owned(), usize::try_from(count).unwrap_or(0)))
    .collect())
}

fn validate_samples(
    connection: &Connection,
    catalog: &LegacyCatalog,
    result: &mut SampleValidation,
) -> MigrationResult<()> {
    let indices = sample_indices(catalog.collections.len(), SAMPLE_LIMIT);
    result.checked = indices.len();
    for index in indices {
        let expected = &catalog.collections[index];
        let actual = connection
            .query_row(
                "SELECT title, event, circle, authors, parody, classification_top,
                        classification_subcategory, is_dl
                 FROM effective_metadata WHERE collection_id = ?1",
                [expected.id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some(actual) = actual else {
            result.mismatches.push(MetadataMismatch {
                collection_id: expected.id,
                field: "effective_metadata".to_owned(),
                expected: json!("present"),
                actual: Value::Null,
            });
            continue;
        };
        let comparisons = [
            ("title", json!(expected.title), json!(actual.0)),
            ("event", json!(expected.event), json!(actual.1)),
            ("circle", json!(expected.circle), json!(actual.2)),
            (
                "authors",
                json!(expected.authors.values.join("、")),
                json!(actual.3),
            ),
            (
                "parody",
                json!(expected.parody.as_ref().map(|value| &value.canonical)),
                json!(actual.4),
            ),
            (
                "classification_top",
                json!(expected.classification.top_level),
                json!(actual.5),
            ),
            (
                "classification_subcategory",
                json!(expected.classification.subcategory),
                json!(actual.6),
            ),
            ("is_dl", json!(expected.is_dl), json!(actual.7 == Some(1))),
        ];
        for (field, expected_value, actual_value) in comparisons {
            if expected_value != actual_value {
                result.mismatches.push(MetadataMismatch {
                    collection_id: expected.id,
                    field: field.to_owned(),
                    expected: expected_value,
                    actual: actual_value,
                });
            }
        }
    }
    Ok(())
}

fn read_target_tags(connection: &Connection) -> MigrationResult<BTreeMap<i64, String>> {
    let mut statement = connection.prepare("SELECT id, name FROM tags ORDER BY id")?;
    Ok(statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?)
}

fn read_target_tag_links(connection: &Connection) -> MigrationResult<BTreeSet<(i64, i64)>> {
    let mut statement = connection.prepare(
        "SELECT collection_id, tag_id FROM collection_tags ORDER BY collection_id, tag_id",
    )?;
    Ok(statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?)
}

fn symmetric_map_mismatches<K: Ord, V: PartialEq>(
    left: &BTreeMap<K, V>,
    right: &BTreeMap<K, V>,
) -> usize {
    left.iter()
        .filter(|(key, value)| right.get(*key) != Some(*value))
        .count()
        + right.keys().filter(|key| !left.contains_key(*key)).count()
}

fn find_path_conflicts(rows: &[LegacyRow]) -> Vec<PathConflict> {
    let mut groups: BTreeMap<String, Vec<(i64, String)>> = BTreeMap::new();
    for row in rows {
        if let Some(path) = nonempty_text(row.filepath.as_deref()) {
            groups
                .entry(normalized_path(path))
                .or_default()
                .push((row.id, path.to_owned()));
        }
    }
    groups
        .into_iter()
        .filter(|(_, rows)| rows.len() > 1)
        .map(|(normalized_path, rows)| PathConflict {
            normalized_path,
            legacy_ids: rows.iter().map(|(id, _)| *id).collect(),
            paths: rows.into_iter().map(|(_, path)| path).collect(),
        })
        .collect()
}

fn source_empty_counts(rows: &[LegacyRow]) -> BTreeMap<String, usize> {
    let fields = [
        "filename",
        "filepath",
        "folder",
        "event",
        "circle",
        "author",
        "title",
        "parody",
        "category",
        "source",
        "created_at",
        "updated_at",
        "is_dl",
    ];
    let mut counts = fields
        .into_iter()
        .map(|field| (field.to_owned(), 0))
        .collect::<BTreeMap<_, _>>();
    for row in rows {
        for (field, value) in [
            ("filename", row.filename.as_deref()),
            ("filepath", row.filepath.as_deref()),
            ("folder", row.folder.as_deref()),
            ("event", row.event.as_deref()),
            ("circle", row.circle.as_deref()),
            ("author", row.author.as_deref()),
            ("title", row.title.as_deref()),
            ("parody", row.parody.as_deref()),
            ("category", row.category.as_deref()),
            ("source", row.source.as_deref()),
            ("created_at", row.created_at.as_deref()),
            ("updated_at", row.updated_at.as_deref()),
        ] {
            if nonempty_text(value).is_none() {
                *counts.get_mut(field).expect("known source field") += 1;
            }
        }
    }
    counts
}

fn legacy_classification(value: &str) -> Classification {
    let (top_level, subcategory) = match value {
        "同人誌" => ("同人誌", None),
        "CG" | "同人CG" | "同人CG集" => ("CG", None),
        "商業誌" => ("商業誌", None),
        "成年コミック" | "エロ漫画" | "アダルトコミック" => {
            ("商業誌", Some("成年コミック"))
        }
        "官能小説・エロライトノベル" | "官能小説" | "エロライトノベル" => {
            ("商業誌", Some("官能小説"))
        }
        "一般コミック" => ("商業誌", Some("一般コミック")),
        _ => ("其他", None),
    };
    Classification {
        top_level: top_level.to_owned(),
        subcategory: subcategory.map(str::to_owned),
        raw_marker: Some(value.to_owned()),
    }
}

fn split_authors(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| value.split(['、', ',']))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn required_text(
    value: &Option<String>,
    field: &str,
    id: i64,
    issues: &mut Vec<BlockingIssue>,
) -> Option<String> {
    match nonempty_text(value.as_deref()) {
        Some(value) => Some(value.to_owned()),
        None => {
            issues.push(BlockingIssue {
                kind: format!("missing_{field}"),
                legacy_id: Some(id),
                detail: format!("doujinshi.{field} 為 NULL 或空白"),
            });
            None
        }
    }
}

fn match_root<'a>(filepath: &str, roots: &'a [RootSetting]) -> Option<(&'a RootSetting, String)> {
    roots
        .iter()
        .filter_map(|root| relative_path(filepath, &root.path).map(|relative| (root, relative)))
        .max_by_key(|(root, _)| normalized_path(&root.path).len())
}

fn relative_path(filepath: &str, root: &str) -> Option<String> {
    let path = filepath.replace('/', "\\");
    let root = root.replace('/', "\\");
    let root = root.trim_end_matches('\\');
    let path_key = path.to_lowercase();
    let root_key = root.to_lowercase();
    if path_key == root_key {
        return None;
    }
    let prefix = format!("{root_key}\\");
    path_key
        .starts_with(&prefix)
        .then(|| path[root.len() + 1..].to_owned())
}

fn filename_from_path(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_owned()
}

fn legacy_media_kind(filepath: Option<&str>) -> LegacyMediaKind {
    let is_zip = filepath
        .and_then(|path| Path::new(path).extension())
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
    if is_zip {
        LegacyMediaKind::Zip
    } else {
        LegacyMediaKind::ImageFolder
    }
}

fn parse_root_source(value: &str) -> Option<LegacyRootSource> {
    match value {
        "archive" => Some(LegacyRootSource::Archive),
        "downloads" => Some(LegacyRootSource::Downloads),
        _ => None,
    }
}

fn normalized_path(value: &str) -> String {
    path_key(Path::new(value))
}

fn nonempty_text(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn nonempty_owned(value: Option<&str>) -> Option<String> {
    nonempty_text(value).map(str::to_owned)
}

fn sample_indices(len: usize, limit: usize) -> Vec<usize> {
    if len == 0 || limit == 0 {
        return Vec::new();
    }
    if len <= limit {
        return (0..len).collect();
    }
    (0..limit)
        .map(|index| index * (len - 1) / (limit - 1))
        .collect()
}

fn finish_source_fingerprint(source: &Path, report: &mut MigrationReport) -> MigrationResult<()> {
    let after_hash = blake3_file(source)?;
    report.source_fingerprint.unchanged = report.source_fingerprint.before_blake3 == after_hash;
    report.source_fingerprint.after_blake3 = after_hash;
    Ok(())
}

fn blake3_file(path: &Path) -> MigrationResult<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn absolute_existing_file(path: &Path) -> MigrationResult<PathBuf> {
    if !path.is_file() {
        return Err(MigrationError::MissingSource(path.to_owned()));
    }
    Ok(fs::canonicalize(path)?)
}

fn absolute_target(path: &Path) -> MigrationResult<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute.parent().ok_or_else(|| {
        MigrationError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target 沒有 parent directory",
        ))
    })?;
    if !parent.is_dir() {
        return Err(MigrationError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("target parent 不存在：{}", parent.display()),
        )));
    }
    Ok(absolute)
}

fn reject_existing_target_artifacts(target: &Path) -> MigrationResult<()> {
    let existing = target_artifacts(target)
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    if existing.is_empty() {
        Ok(())
    } else {
        Err(MigrationError::TargetAlreadyExists(existing))
    }
}

fn reject_source_sidecars(source: &Path) -> MigrationResult<()> {
    let existing = [appended_path(source, "-wal"), appended_path(source, "-shm")]
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    if existing.is_empty() {
        Ok(())
    } else {
        Err(MigrationError::SourceHasSidecars(existing))
    }
}

fn immutable_sqlite_uri(path: &Path) -> MigrationResult<String> {
    let text = path.to_str().ok_or_else(|| {
        MigrationError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("SQLite source path 不是有效 Unicode：{}", path.display()),
        ))
    })?;
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

fn target_artifacts(target: &Path) -> [PathBuf; 3] {
    [
        target.to_owned(),
        appended_path(target, "-wal"),
        appended_path(target, "-shm"),
    ]
}

fn appended_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn remove_new_target_artifacts(target: &Path) {
    for path in target_artifacts(target) {
        let _ = fs::remove_file(path);
    }
}
