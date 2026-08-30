use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use doujin_parser::PARSER_VERSION;
use doujin_parser::domain::{ParseInput, ParseResult, ParseStatus};
use doujin_parser::filename::{
    RenameOutcome, decode_percent_encoded_filename, normalize_new_collection_zip,
    plan_new_collection_zip,
};
use doujin_parser::parser::parse_filename;

const EXCLUDED_DIRECTORIES: &[&str] = &[
    "templates",
    "static",
    "__pycache__",
    ".git",
    "node_modules",
    "$RECYCLE.BIN",
    "System Volume Information",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Zip,
    ImageFolder,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::ImageFolder => "image_folder",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "zip" => Some(Self::Zip),
            "image_folder" => Some(Self::ImageFolder),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Archive,
    Downloads,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRoot {
    pub path: PathBuf,
    pub source: SourceKind,
    pub label: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScanMode {
    #[default]
    ApplyRenames,
    DryRun,
    NoRename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilenameNormalization {
    Unchanged,
    Renamed { original: PathBuf, renamed: PathBuf },
    PlannedRename { original: PathBuf, renamed: PathBuf },
    KeptOriginal { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCollection {
    pub path: PathBuf,
    pub folder: PathBuf,
    pub root_path: PathBuf,
    pub root_label: String,
    pub source: SourceKind,
    pub parser_version: String,
    pub parsed: ParseResult,
    pub filename_normalization: FilenameNormalization,
    pub media_kind: MediaKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanIssueKind {
    NoRoots,
    MissingRoot,
    ReadDirectory,
    ReadEntry,
    NonUnicodeFilename,
    MediaKindMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanIssue {
    pub path: PathBuf,
    pub kind: ScanIssueKind,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub roots: usize,
    pub missing_roots: usize,
    pub discovered: usize,
    pub pending: usize,
    pub skipped_existing: usize,
    pub renamed: usize,
    pub planned_renames: usize,
    pub normalization_warnings: usize,
    pub parse_complete: usize,
    pub parse_partial: usize,
    pub parse_title_only: usize,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOutput {
    pub pending: Vec<PendingCollection>,
    pub issues: Vec<ScanIssue>,
    pub summary: ScanSummary,
}

pub fn scan_new_collections(
    roots: &[ScanRoot],
    known_paths: &HashMap<PathBuf, MediaKind>,
) -> ScanOutput {
    scan_new_collections_with_mode(roots, known_paths, ScanMode::ApplyRenames)
}

pub fn scan_new_collections_with_mode(
    roots: &[ScanRoot],
    known_paths: &HashMap<PathBuf, MediaKind>,
    mode: ScanMode,
) -> ScanOutput {
    let started = Instant::now();
    let mut output = ScanOutput {
        pending: Vec::new(),
        issues: Vec::new(),
        summary: ScanSummary {
            roots: roots.len(),
            ..ScanSummary::default()
        },
    };

    if roots.is_empty() {
        output.issues.push(ScanIssue {
            path: PathBuf::new(),
            kind: ScanIssueKind::NoRoots,
            message: "尚未設定掃描來源".to_owned(),
        });
        output.summary.elapsed_ms = started.elapsed().as_millis();
        return output;
    }

    for root in roots {
        if !root.path.is_dir() {
            output.summary.missing_roots += 1;
            output.issues.push(ScanIssue {
                path: root.path.clone(),
                kind: ScanIssueKind::MissingRoot,
                message: format!("掃描來源不存在或不是資料夾：{}", root.label),
            });
            continue;
        }
        scan_directory(root, &root.path, known_paths, mode, &mut output, true);
    }

    output
        .pending
        .sort_by(|left, right| left.path.cmp(&right.path));
    output
        .issues
        .sort_by(|left, right| left.path.cmp(&right.path));
    output.summary.pending = output.pending.len();
    output.summary.elapsed_ms = started.elapsed().as_millis();
    output
}

fn scan_directory(
    root: &ScanRoot,
    directory: &Path,
    known_paths: &HashMap<PathBuf, MediaKind>,
    mode: ScanMode,
    output: &mut ScanOutput,
    is_root: bool,
) {
    if !is_root {
        match known_paths.get(directory) {
            Some(MediaKind::ImageFolder) => {
                output.summary.discovered += 1;
                output.summary.skipped_existing += 1;
                return;
            }
            Some(MediaKind::Zip) => {
                output.summary.discovered += 1;
                output.issues.push(ScanIssue {
                    path: directory.to_owned(),
                    kind: ScanIssueKind::MediaKindMismatch,
                    message: "收藏索引記錄為 ZIP 檔案，實際為資料夾".to_owned(),
                });
                return;
            }
            None => {}
        }
    }

    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            output.issues.push(ScanIssue {
                path: directory.to_owned(),
                kind: ScanIssueKind::ReadDirectory,
                message: error.to_string(),
            });
            return;
        }
    };

    let mut entries = entries.collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.as_ref().ok().map(fs::DirEntry::path));
    let mut children = Vec::with_capacity(entries.len());
    let mut children_complete = true;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                children_complete = false;
                output.issues.push(ScanIssue {
                    path: directory.to_owned(),
                    kind: ScanIssueKind::ReadEntry,
                    message: error.to_string(),
                });
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                children_complete = false;
                output.issues.push(ScanIssue {
                    path,
                    kind: ScanIssueKind::ReadEntry,
                    message: error.to_string(),
                });
                continue;
            }
        };
        children.push((entry.file_name(), path, file_type));
    }

    if !is_root && is_variant_parent(children_complete, &children, known_paths) {
        push_image_folder(root, directory, output);
        return;
    }

    if !is_root && is_image_folder_boundary(children_complete, &children) {
        push_image_folder(root, directory, output);
        return;
    }

    for (name, path, file_type) in children {
        if file_type.is_dir() {
            if !is_excluded_directory(&name) {
                scan_directory(root, &path, known_paths, mode, output, false);
            }
        } else if file_type.is_file() && is_zip(&path) {
            process_zip(root, path, known_paths, mode, output);
        }
    }
}

/// 邊界判定只在目錄列舉完整時成立：少讀到一個 entry 就可能把「有子資料夾或 ZIP 的
/// 中間層」誤判成收藏本體，因此列舉不完整時一律回傳 `false`，改走遞迴／ZIP 路徑。
fn is_image_folder_boundary(
    children_complete: bool,
    children: &[(OsString, PathBuf, fs::FileType)],
) -> bool {
    if !children_complete {
        return false;
    }
    let has_zip = children
        .iter()
        .any(|(_, path, file_type)| file_type.is_file() && is_zip(path));
    if has_zip {
        return false;
    }
    children
        .iter()
        .any(|(_, path, file_type)| file_type.is_file() && is_supported_image_extension(path))
}

/// 同 `is_image_folder_boundary`：列舉不完整時不做判定。
fn is_variant_parent(
    children_complete: bool,
    children: &[(OsString, PathBuf, fs::FileType)],
    known_paths: &HashMap<PathBuf, MediaKind>,
) -> bool {
    if !children_complete {
        return false;
    }
    let has_media_file = children.iter().any(|(_, path, file_type)| {
        file_type.is_file() && (is_supported_image_extension(path) || is_zip(path))
    });
    if has_media_file {
        return false;
    }

    let subdirectories = children
        .iter()
        .filter(|(name, _, file_type)| file_type.is_dir() && !is_excluded_directory(name))
        .collect::<Vec<_>>();
    if !(2..=4).contains(&subdirectories.len()) {
        return false;
    }

    subdirectories.iter().all(|(name, path, _)| {
        name.to_str()
            .is_some_and(|name| !name.contains(['[', '(', '［', '（']))
            && !known_paths.contains_key(path)
            && is_leaf_image_folder(path)
    })
}

fn is_leaf_image_folder(directory: &Path) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    let mut has_image = false;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_dir() {
            return false;
        }
        if file_type.is_file() {
            let path = entry.path();
            if is_zip(&path) {
                return false;
            }
            if is_supported_image_extension(&path) {
                has_image = true;
            }
        }
    }
    has_image
}

fn push_image_folder(root: &ScanRoot, directory: &Path, output: &mut ScanOutput) {
    output.summary.discovered += 1;
    let Some(raw_name) = directory.file_name().and_then(|name| name.to_str()) else {
        output.issues.push(ScanIssue {
            path: directory.to_owned(),
            kind: ScanIssueKind::NonUnicodeFilename,
            message: "檔名不是有效的 Unicode".to_owned(),
        });
        return;
    };
    let parse_name = match decode_percent_encoded_filename(raw_name) {
        Ok(Some(decoded)) if !decoded.contains(['/', '\\']) => decoded,
        _ => raw_name.to_owned(),
    };
    let parsed = parse_filename(&ParseInput {
        filename: parse_name,
        parody_evidence: Vec::new(),
    });
    match parsed.parse_status {
        ParseStatus::Complete => output.summary.parse_complete += 1,
        ParseStatus::Partial => output.summary.parse_partial += 1,
        ParseStatus::TitleOnly => output.summary.parse_title_only += 1,
    }

    output.pending.push(PendingCollection {
        folder: directory.parent().unwrap_or(Path::new("")).to_owned(),
        path: directory.to_owned(),
        root_path: root.path.clone(),
        root_label: root.label.clone(),
        source: root.source,
        parser_version: PARSER_VERSION.to_owned(),
        parsed,
        filename_normalization: FilenameNormalization::Unchanged,
        media_kind: MediaKind::ImageFolder,
    });
}

fn process_zip(
    root: &ScanRoot,
    original_path: PathBuf,
    known_paths: &HashMap<PathBuf, MediaKind>,
    mode: ScanMode,
    output: &mut ScanOutput,
) {
    output.summary.discovered += 1;
    match known_paths.get(&original_path) {
        Some(MediaKind::Zip) => {
            output.summary.skipped_existing += 1;
            return;
        }
        Some(MediaKind::ImageFolder) => {
            output.issues.push(ScanIssue {
                path: original_path,
                kind: ScanIssueKind::MediaKindMismatch,
                message: "收藏索引記錄為圖片資料夾，實際為 ZIP 檔案".to_owned(),
            });
            return;
        }
        None => {}
    }

    let indexed_target = indexed_decoded_target(&original_path, known_paths);
    let (path, parse_path, filename_normalization) = if let Some(target) = indexed_target {
        (
            original_path.clone(),
            original_path,
            FilenameNormalization::KeptOriginal {
                reason: format!("解碼後路徑已存在於收藏索引：{}", target.display()),
            },
        )
    } else {
        let outcome = match mode {
            ScanMode::ApplyRenames => normalize_new_collection_zip(&original_path, Vec::new()),
            ScanMode::DryRun | ScanMode::NoRename => {
                plan_new_collection_zip(&original_path, Vec::new())
            }
        };
        match outcome {
            Ok(RenameOutcome::NotPercentEncoded) => (
                original_path.clone(),
                original_path,
                FilenameNormalization::Unchanged,
            ),
            Ok(RenameOutcome::NotStructurallyParsed { decoded_filename }) => (
                original_path.clone(),
                original_path,
                FilenameNormalization::KeptOriginal {
                    reason: format!("解碼後未解析出收藏結構：{decoded_filename}"),
                },
            ),
            Ok(RenameOutcome::Renamed { original, renamed }) => {
                output.summary.planned_renames += 1;
                match mode {
                    ScanMode::ApplyRenames => {
                        output.summary.renamed += 1;
                        (
                            renamed.clone(),
                            renamed.clone(),
                            FilenameNormalization::Renamed { original, renamed },
                        )
                    }
                    ScanMode::DryRun => (
                        original.clone(),
                        renamed.clone(),
                        FilenameNormalization::PlannedRename { original, renamed },
                    ),
                    ScanMode::NoRename => (
                        original.clone(),
                        original.clone(),
                        FilenameNormalization::PlannedRename { original, renamed },
                    ),
                }
            }
            Err(error) => (
                original_path.clone(),
                original_path,
                FilenameNormalization::KeptOriginal {
                    reason: error.to_string(),
                },
            ),
        }
    };

    if matches!(
        filename_normalization,
        FilenameNormalization::KeptOriginal { .. }
    ) {
        output.summary.normalization_warnings += 1;
    }

    let Some(filename) = parse_path
        .file_name()
        .and_then(|filename| filename.to_str())
    else {
        output.issues.push(ScanIssue {
            path,
            kind: ScanIssueKind::NonUnicodeFilename,
            message: "檔名不是有效的 Unicode".to_owned(),
        });
        return;
    };
    let parsed = parse_filename(&ParseInput {
        filename: filename.to_owned(),
        parody_evidence: Vec::new(),
    });
    match parsed.parse_status {
        ParseStatus::Complete => output.summary.parse_complete += 1,
        ParseStatus::Partial => output.summary.parse_partial += 1,
        ParseStatus::TitleOnly => output.summary.parse_title_only += 1,
    }

    output.pending.push(PendingCollection {
        folder: path.parent().unwrap_or(Path::new("")).to_owned(),
        path,
        root_path: root.path.clone(),
        root_label: root.label.clone(),
        source: root.source,
        parser_version: PARSER_VERSION.to_owned(),
        parsed,
        filename_normalization,
        media_kind: MediaKind::Zip,
    });
}

fn is_excluded_directory(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        EXCLUDED_DIRECTORIES
            .iter()
            .any(|excluded| name.eq_ignore_ascii_case(excluded))
    })
}

pub fn is_supported_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp"
            )
        })
}

fn is_zip(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

fn indexed_decoded_target(
    original_path: &Path,
    known_paths: &HashMap<PathBuf, MediaKind>,
) -> Option<PathBuf> {
    let filename = original_path.file_name()?.to_str()?;
    let decoded = decode_percent_encoded_filename(filename).ok()??;
    if decoded.contains(['/', '\\']) {
        return None;
    }
    let target = original_path.with_file_name(decoded);
    known_paths.contains_key(&target).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "doujin-scanner-unit-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test directory");
        path
    }

    fn children_of(directory: &Path) -> Vec<(OsString, PathBuf, fs::FileType)> {
        let mut children = fs::read_dir(directory)
            .expect("read directory")
            .map(|entry| {
                let entry = entry.expect("directory entry");
                let file_type = entry.file_type().expect("entry file type");
                (entry.file_name(), entry.path(), file_type)
            })
            .collect::<Vec<_>>();
        children.sort_by(|left, right| left.1.cmp(&right.1));
        children
    }

    /// 目錄列舉不完整時（`DirEntry` 或 `file_type()` 失敗）兩個邊界判定都必須 fail-closed：
    /// 同一組 children 只有在 `children_complete` 為真時才判定成立。
    #[test]
    fn boundary_decisions_are_skipped_when_the_directory_listing_is_incomplete() {
        let root = test_directory("incomplete-listing");
        let parent = root.join("variant work");
        for directory in ["Text", "Textless"] {
            let leaf = parent.join(directory);
            fs::create_dir_all(&leaf).expect("create leaf directory");
            fs::write(leaf.join("001.png"), b"").expect("write leaf image");
        }
        let known_paths = HashMap::new();

        let parent_children = children_of(&parent);
        assert!(is_variant_parent(true, &parent_children, &known_paths));
        assert!(!is_variant_parent(false, &parent_children, &known_paths));

        let leaf_children = children_of(&parent.join("Text"));
        assert!(is_image_folder_boundary(true, &leaf_children));
        assert!(!is_image_folder_boundary(false, &leaf_children));

        fs::remove_dir_all(root).expect("remove test directory");
    }
}
