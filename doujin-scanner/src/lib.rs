use std::collections::HashSet;
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanIssueKind {
    NoRoots,
    MissingRoot,
    ReadDirectory,
    ReadEntry,
    NonUnicodeFilename,
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

pub fn scan_new_collections(roots: &[ScanRoot], existing_paths: &HashSet<PathBuf>) -> ScanOutput {
    scan_new_collections_with_mode(roots, existing_paths, ScanMode::ApplyRenames)
}

pub fn scan_new_collections_with_mode(
    roots: &[ScanRoot],
    existing_paths: &HashSet<PathBuf>,
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
        scan_directory(root, &root.path, existing_paths, mode, &mut output);
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
    existing_paths: &HashSet<PathBuf>,
    mode: ScanMode,
    output: &mut ScanOutput,
) {
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
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
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
                output.issues.push(ScanIssue {
                    path,
                    kind: ScanIssueKind::ReadEntry,
                    message: error.to_string(),
                });
                continue;
            }
        };

        if file_type.is_dir() {
            if !is_excluded_directory(&entry.file_name()) {
                scan_directory(root, &entry.path(), existing_paths, mode, output);
            }
        } else if file_type.is_file() && is_zip(&path) {
            process_zip(root, path, existing_paths, mode, output);
        }
    }
}

fn process_zip(
    root: &ScanRoot,
    original_path: PathBuf,
    existing_paths: &HashSet<PathBuf>,
    mode: ScanMode,
    output: &mut ScanOutput,
) {
    output.summary.discovered += 1;
    if existing_paths.contains(&original_path) {
        output.summary.skipped_existing += 1;
        return;
    }

    let indexed_target = indexed_decoded_target(&original_path, existing_paths);
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
    });
}

fn is_excluded_directory(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        EXCLUDED_DIRECTORIES
            .iter()
            .any(|excluded| name.eq_ignore_ascii_case(excluded))
    })
}

fn is_zip(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

fn indexed_decoded_target(
    original_path: &Path,
    existing_paths: &HashSet<PathBuf>,
) -> Option<PathBuf> {
    let filename = original_path.file_name()?.to_str()?;
    let decoded = decode_percent_encoded_filename(filename).ok()??;
    if decoded.contains(['/', '\\']) {
        return None;
    }
    let target = original_path.with_file_name(decoded);
    existing_paths.contains(&target).then_some(target)
}
