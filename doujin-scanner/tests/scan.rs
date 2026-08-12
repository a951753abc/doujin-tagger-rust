use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use doujin_parser::PARSER_VERSION;
use doujin_scanner::{
    FilenameNormalization, ScanIssueKind, ScanRoot, SourceKind, scan_new_collections,
};

struct TestTree {
    path: PathBuf,
}

impl TestTree {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("doujin-scanner-{label}-{unique}"));
        fs::create_dir(&path).expect("create test root");
        Self { path }
    }

    fn zip(&self, relative: &str) -> PathBuf {
        let path = self.path.join(relative);
        fs::create_dir_all(path.parent().expect("zip parent")).expect("create zip parent");
        fs::write(&path, b"zip placeholder").expect("create zip");
        path
    }

    fn root(&self, source: SourceKind) -> ScanRoot {
        ScanRoot {
            path: self.path.clone(),
            source,
            label: "測試來源".to_owned(),
        }
    }
}

impl Drop for TestTree {
    fn drop(&mut self) {
        if self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("doujin-scanner-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[test]
fn no_roots_returns_an_explicit_issue_without_changes() {
    let output = scan_new_collections(&[], &HashSet::new());

    assert!(output.pending.is_empty());
    assert_eq!(0, output.summary.discovered);
    assert_eq!(ScanIssueKind::NoRoots, output.issues[0].kind);
}

#[test]
fn missing_root_is_skipped_while_existing_root_is_scanned() {
    let tree = TestTree::new("missing-root");
    let zip = tree.zip("nested/[circle] title.zip");
    let missing = tree.path.join("missing");
    let roots = [
        ScanRoot {
            path: missing.clone(),
            source: SourceKind::Downloads,
            label: "不存在".to_owned(),
        },
        tree.root(SourceKind::Archive),
    ];

    let output = scan_new_collections(&roots, &HashSet::new());

    assert_eq!(1, output.summary.missing_roots);
    assert_eq!(1, output.summary.pending);
    assert_eq!(zip, output.pending[0].path);
    assert!(
        output
            .issues
            .iter()
            .any(|issue| issue.kind == ScanIssueKind::MissingRoot && issue.path == missing)
    );
}

#[test]
fn recursive_scan_skips_existing_paths_and_excluded_directories() {
    let tree = TestTree::new("recursive");
    let existing = tree.zip("existing.zip");
    let new_zip = tree.zip("nested/[circle] new.zip");
    tree.zip(".git/hidden.zip");
    tree.zip("node_modules/hidden.zip");
    let existing_paths = HashSet::from([existing]);

    let output = scan_new_collections(&[tree.root(SourceKind::Downloads)], &existing_paths);

    assert_eq!(2, output.summary.discovered);
    assert_eq!(1, output.summary.skipped_existing);
    assert_eq!(1, output.summary.pending);
    assert_eq!(new_zip, output.pending[0].path);
    assert_eq!(tree.path, output.pending[0].root_path);
    assert_eq!("測試來源", output.pending[0].root_label);
    assert_eq!(SourceKind::Downloads, output.pending[0].source);
    assert_eq!(new_zip.parent().expect("parent"), output.pending[0].folder);
    assert_eq!(
        "circle",
        output.pending[0].parsed.circle.as_deref().unwrap()
    );
}

#[test]
fn existing_percent_encoded_path_is_not_reparsed_or_renamed() {
    let tree = TestTree::new("existing-encoded");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let decoded = tree.path.join("(C77) [circle] title.zip");
    let existing_paths = HashSet::from([original.clone()]);

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &existing_paths);

    assert_eq!(1, output.summary.skipped_existing);
    assert!(output.pending.is_empty());
    assert!(original.exists());
    assert!(!decoded.exists());
}

#[test]
fn new_percent_encoded_zip_is_renamed_before_becoming_pending() {
    let tree = TestTree::new("new-encoded");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let decoded = tree.path.join("(C77) [circle] title.zip");

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashSet::new());

    assert_eq!(1, output.summary.renamed);
    assert_eq!(decoded, output.pending[0].path);
    assert_eq!(Some("C77"), output.pending[0].parsed.event.as_deref());
    assert_eq!(
        FilenameNormalization::Renamed {
            original: original.clone(),
            renamed: decoded.clone(),
        },
        output.pending[0].filename_normalization
    );
    assert!(!original.exists());
    assert!(decoded.exists());
}

#[test]
fn normalization_collision_keeps_original_and_returns_a_pending_warning() {
    let tree = TestTree::new("collision");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let decoded = tree.zip("(C77) [circle] title.zip");
    let existing_paths = HashSet::from([decoded]);

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &existing_paths);

    assert_eq!(1, output.summary.normalization_warnings);
    assert_eq!(1, output.summary.skipped_existing);
    assert_eq!(1, output.summary.pending);
    assert_eq!(original, output.pending[0].path);
    assert!(matches!(
        output.pending[0].filename_normalization,
        FilenameNormalization::KeptOriginal { .. }
    ));
    assert!(original.exists());
}

#[test]
fn decoded_path_already_in_index_is_not_used_even_when_target_file_is_missing() {
    let tree = TestTree::new("indexed-collision");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let decoded = tree.path.join("(C77) [circle] title.zip");
    let existing_paths = HashSet::from([decoded.clone()]);

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &existing_paths);

    assert_eq!(1, output.summary.normalization_warnings);
    assert_eq!(original, output.pending[0].path);
    assert!(original.exists());
    assert!(!decoded.exists());
}

#[test]
fn parser_version_is_recorded_for_new_collections() {
    let tree = TestTree::new("parser-version");
    tree.zip("[circle] title.zip");

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashSet::new());

    assert_eq!(PARSER_VERSION, output.pending[0].parser_version);
}
