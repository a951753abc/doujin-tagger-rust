use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use doujin_parser::PARSER_VERSION;
use doujin_scanner::{
    FilenameNormalization, MediaKind, ScanIssueKind, ScanMode, ScanRoot, SourceKind,
    scan_new_collections, scan_new_collections_with_mode,
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

    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.path.join(relative);
        fs::create_dir_all(&path).expect("create directory");
        path
    }

    fn image(&self, relative: &str) -> PathBuf {
        self.write_file(relative, b"image placeholder")
    }

    fn plain_file(&self, relative: &str) -> PathBuf {
        self.write_file(relative, b"file placeholder")
    }

    fn write_file(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.path.join(relative);
        fs::create_dir_all(path.parent().expect("file parent")).expect("create file parent");
        fs::write(&path, contents).expect("create file");
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
    let output = scan_new_collections(&[], &HashMap::new());

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

    let output = scan_new_collections(&roots, &HashMap::new());

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
    let existing_paths = HashMap::from([(existing, MediaKind::Zip)]);

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
    let existing_paths = HashMap::from([(original.clone(), MediaKind::Zip)]);

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

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert_eq!(1, output.summary.renamed);
    assert_eq!(1, output.summary.planned_renames);
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
fn dry_run_reports_rename_diff_without_changing_the_file() {
    let tree = TestTree::new("dry-run-encoded");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let decoded = tree.path.join("(C77) [circle] title.zip");

    let output = scan_new_collections_with_mode(
        &[tree.root(SourceKind::Archive)],
        &HashMap::new(),
        ScanMode::DryRun,
    );

    assert_eq!(0, output.summary.renamed);
    assert_eq!(1, output.summary.planned_renames);
    assert_eq!(original, output.pending[0].path);
    assert_eq!(Some("C77"), output.pending[0].parsed.event.as_deref());
    assert_eq!(
        FilenameNormalization::PlannedRename {
            original: original.clone(),
            renamed: decoded.clone(),
        },
        output.pending[0].filename_normalization
    );
    assert!(original.exists());
    assert!(!decoded.exists());
}

#[test]
fn no_rename_uses_the_real_original_path_and_metadata_input() {
    let tree = TestTree::new("no-rename-encoded");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let decoded = tree.path.join("(C77) [circle] title.zip");

    let output = scan_new_collections_with_mode(
        &[tree.root(SourceKind::Archive)],
        &HashMap::new(),
        ScanMode::NoRename,
    );

    assert_eq!(original, output.pending[0].path);
    assert_eq!(0, output.summary.renamed);
    assert_eq!(1, output.summary.planned_renames);
    assert!(original.exists());
    assert!(!decoded.exists());
}

#[test]
fn normalization_collision_keeps_original_and_returns_a_pending_warning() {
    let tree = TestTree::new("collision");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let decoded = tree.zip("(C77) [circle] title.zip");
    let existing_paths = HashMap::from([(decoded, MediaKind::Zip)]);

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
    let existing_paths = HashMap::from([(decoded.clone(), MediaKind::Zip)]);

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

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert_eq!(PARSER_VERSION, output.pending[0].parser_version);
}

#[test]
fn image_folder_is_discovered_as_a_single_pending_collection_in_every_mode() {
    for mode in [ScanMode::ApplyRenames, ScanMode::DryRun, ScanMode::NoRename] {
        let tree = TestTree::new("image-folder");
        let folder = tree.dir("[circle] folder title");
        tree.image("[circle] folder title/001.JPG");
        tree.image("[circle] folder title/002.png");
        tree.image("[circle] folder title/cover.WebP");
        tree.plain_file("[circle] folder title/notes.txt");
        tree.image("[circle] folder title/extras/003.gif");
        let inner_zip = tree.zip("[circle] folder title/deep/[c] inner.zip");

        let output = scan_new_collections_with_mode(
            &[tree.root(SourceKind::Archive)],
            &HashMap::new(),
            mode,
        );

        assert_eq!(1, output.summary.discovered);
        assert_eq!(1, output.summary.pending);
        assert_eq!(0, output.summary.renamed);
        assert_eq!(0, output.summary.planned_renames);
        assert_eq!(1, output.pending.len());
        assert_eq!(folder, output.pending[0].path);
        assert_eq!(MediaKind::ImageFolder, output.pending[0].media_kind);
        assert_eq!(tree.path, output.pending[0].folder);
        assert_eq!(
            FilenameNormalization::Unchanged,
            output.pending[0].filename_normalization
        );
        assert_eq!(Some("circle"), output.pending[0].parsed.circle.as_deref());
        assert!(!output.pending.iter().any(|entry| entry.path == inner_zip));
        assert!(folder.is_dir());
        assert!(inner_zip.exists());
    }
}

#[test]
fn every_supported_image_extension_marks_its_folder_while_document_folders_recurse() {
    let tree = TestTree::new("image-extensions");
    let jpg = tree.dir("jpg-folder");
    tree.image("jpg-folder/001.jpg");
    let jpeg = tree.dir("jpeg-folder");
    tree.image("jpeg-folder/001.JPEG");
    let png = tree.dir("png-folder");
    tree.image("png-folder/001.Png");
    let gif = tree.dir("gif-folder");
    tree.image("gif-folder/001.GIF");
    let bmp = tree.dir("bmp-folder");
    tree.image("bmp-folder/001.bMp");
    let webp = tree.dir("webp-folder");
    tree.image("webp-folder/001.WebP");
    let docs_only = tree.dir("docs-only");
    tree.plain_file("docs-only/readme.txt");
    tree.plain_file("docs-only/art.psd");
    let inner_zip = tree.zip("docs-only/inner/[c] x.zip");

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    let folders = output
        .pending
        .iter()
        .filter(|entry| entry.media_kind == MediaKind::ImageFolder)
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(6, folders.len());
    for expected in [jpg, jpeg, png, gif, bmp, webp] {
        assert!(folders.contains(&expected), "缺少 {}", expected.display());
    }
    assert!(!output.pending.iter().any(|entry| entry.path == docs_only));
    assert!(
        output
            .pending
            .iter()
            .any(|entry| entry.path == inner_zip && entry.media_kind == MediaKind::Zip)
    );
    assert_eq!(7, output.summary.pending);
}

#[test]
fn a_zip_sibling_keeps_the_folder_a_container_while_the_image_subfolder_is_a_collection() {
    let tree = TestTree::new("mixed");
    let mixed = tree.dir("mixed");
    tree.image("mixed/a.jpg");
    let zip = tree.zip("mixed/[c] b.zip");
    let sub = tree.dir("mixed/sub");
    tree.image("mixed/sub/001.png");

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert_eq!(2, output.pending.len());
    assert_eq!(2, output.summary.pending);
    assert!(
        output
            .pending
            .iter()
            .any(|entry| entry.path == zip && entry.media_kind == MediaKind::Zip)
    );
    assert!(
        output
            .pending
            .iter()
            .any(|entry| entry.path == sub && entry.media_kind == MediaKind::ImageFolder)
    );
    assert!(!output.pending.iter().any(|entry| entry.path == mixed));
}

#[test]
fn a_registered_root_is_never_a_collection_even_when_it_holds_images() {
    let tree = TestTree::new("root-images");
    tree.image("001.jpg");
    let child = tree.dir("[c] child");
    tree.image("[c] child/001.jpg");

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert_eq!(1, output.pending.len());
    assert_eq!(child, output.pending[0].path);
    assert_eq!(MediaKind::ImageFolder, output.pending[0].media_kind);
    assert!(!output.pending.iter().any(|entry| entry.path == tree.path));
}

#[test]
fn a_known_image_folder_is_skipped_without_descending_into_it() {
    let tree = TestTree::new("known-image-folder");
    let folder = tree.dir("[c] known");
    tree.image("[c] known/001.jpg");
    let sub = tree.dir("[c] known/sub");
    tree.image("[c] known/sub/002.jpg");
    let known = HashMap::from([(folder, MediaKind::ImageFolder)]);

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &known);

    assert!(output.pending.is_empty());
    assert_eq!(1, output.summary.discovered);
    assert_eq!(1, output.summary.skipped_existing);
    assert!(!output.pending.iter().any(|entry| entry.path == sub));
    assert!(output.issues.is_empty());
}

#[test]
fn a_path_indexed_as_zip_but_now_a_directory_reports_a_media_kind_mismatch() {
    let tree = TestTree::new("known-zip-now-folder");
    let path = tree.dir("[c] shifted");
    tree.image("[c] shifted/001.jpg");
    let sub = tree.dir("[c] shifted/sub");
    tree.image("[c] shifted/sub/003.jpg");
    let known = HashMap::from([(path.clone(), MediaKind::Zip)]);

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &known);

    assert!(output.pending.is_empty());
    assert_eq!(1, output.issues.len());
    assert_eq!(ScanIssueKind::MediaKindMismatch, output.issues[0].kind);
    assert_eq!(path, output.issues[0].path);
    assert!(!output.pending.iter().any(|entry| entry.path == sub));
}

#[test]
fn a_path_indexed_as_image_folder_but_now_a_zip_is_reported_without_being_renamed() {
    let tree = TestTree::new("known-folder-now-zip");
    let original = tree.zip("%28C77%29%20%5Bcircle%5D%20title.zip");
    let decoded = tree.path.join("(C77) [circle] title.zip");
    let known = HashMap::from([(original.clone(), MediaKind::ImageFolder)]);

    let output = scan_new_collections_with_mode(
        &[tree.root(SourceKind::Archive)],
        &known,
        ScanMode::ApplyRenames,
    );

    assert!(output.pending.is_empty());
    assert_eq!(1, output.issues.len());
    assert_eq!(ScanIssueKind::MediaKindMismatch, output.issues[0].kind);
    assert_eq!(original, output.issues[0].path);
    assert_eq!(0, output.summary.renamed);
    assert!(original.exists());
    assert!(!decoded.exists());
}

#[test]
fn excluded_directories_never_become_image_folder_collections() {
    let tree = TestTree::new("excluded-images");
    tree.image(".git/001.jpg");
    tree.image("node_modules/x.png");

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert!(output.pending.is_empty());
    assert_eq!(0, output.summary.discovered);
}

#[test]
fn percent_encoded_folder_names_are_parsed_decoded_without_touching_the_directory() {
    let tree = TestTree::new("encoded-folder");
    let folder = tree.dir("%28C77%29%20%5Bcircle%5D%20title");
    tree.image("%28C77%29%20%5Bcircle%5D%20title/001.jpg");
    let decoded = tree.path.join("(C77) [circle] title");

    let output = scan_new_collections_with_mode(
        &[tree.root(SourceKind::Archive)],
        &HashMap::new(),
        ScanMode::ApplyRenames,
    );

    assert_eq!(1, output.pending.len());
    assert_eq!(folder, output.pending[0].path);
    assert_eq!(Some("C77"), output.pending[0].parsed.event.as_deref());
    assert_eq!(
        FilenameNormalization::Unchanged,
        output.pending[0].filename_normalization
    );
    assert_eq!(0, output.summary.planned_renames);
    assert_eq!(0, output.summary.normalization_warnings);
    assert_eq!(0, output.summary.renamed);
    assert!(folder.is_dir());
    assert!(!decoded.exists());
}

#[cfg(windows)]
#[test]
fn non_unicode_folder_names_report_an_issue_without_pending() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    let tree = TestTree::new("non-unicode-folder");
    let name = OsString::from_wide(&[0xD800, 0x0066, 0x006F]);
    let folder = tree.path.join(&name);
    if fs::create_dir(&folder).is_err() {
        return;
    }
    if fs::write(folder.join("001.jpg"), b"image placeholder").is_err() {
        return;
    }

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert!(output.pending.is_empty());
    assert_eq!(1, output.issues.len());
    assert_eq!(ScanIssueKind::NonUnicodeFilename, output.issues[0].kind);
    assert_eq!(folder, output.issues[0].path);
}

#[test]
fn a_variant_parent_becomes_a_single_pending_collection_in_every_mode() {
    for mode in [ScanMode::ApplyRenames, ScanMode::DryRun, ScanMode::NoRename] {
        let tree = TestTree::new("variant-parent");
        let parent = tree.dir("[circle] variants");
        let first = tree.image("[circle] variants/Text/Text_001.jpg");
        let second = tree.image("[circle] variants/Text/Text_002.jpg");
        let third = tree.image("[circle] variants/Textless/Textless_001.jpg");
        let notes = tree.plain_file("[circle] variants/readme.txt");

        let output = scan_new_collections_with_mode(
            &[tree.root(SourceKind::Archive)],
            &HashMap::new(),
            mode,
        );

        assert_eq!(1, output.pending.len(), "模式 {mode:?}");
        assert_eq!(parent, output.pending[0].path);
        assert_eq!(MediaKind::ImageFolder, output.pending[0].media_kind);
        assert_eq!(
            FilenameNormalization::Unchanged,
            output.pending[0].filename_normalization
        );
        assert_eq!(Some("circle"), output.pending[0].parsed.circle.as_deref());
        assert_eq!(1, output.summary.discovered);
        assert_eq!(1, output.summary.pending);
        assert_eq!(0, output.summary.renamed);
        assert_eq!(0, output.summary.planned_renames);
        assert!(first.exists());
        assert!(second.exists());
        assert!(third.exists());
        assert!(notes.exists());
        assert_eq!(
            3,
            fs::read_dir(&parent).expect("read variant parent").count()
        );
    }
}

#[test]
fn subdirectory_names_with_brackets_fall_back_to_the_existing_rule() {
    let tree = TestTree::new("variant-brackets");
    let parent = tree.dir("parent");
    let work_a = tree.dir("parent/[c] work a");
    tree.image("parent/[c] work a/001.jpg");
    let work_b = tree.dir("parent/[c] work b");
    tree.image("parent/[c] work b/001.jpg");

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert_eq!(2, output.pending.len());
    assert_eq!(2, output.summary.pending);
    assert!(output.pending.iter().any(|entry| entry.path == work_a));
    assert!(output.pending.iter().any(|entry| entry.path == work_b));
    assert!(!output.pending.iter().any(|entry| entry.path == parent));
}

#[test]
fn variant_parent_requires_between_two_and_four_subdirectories() {
    let tree = TestTree::new("variant-count");
    let wrap = tree.dir("wrap");
    let inner = tree.dir("wrap/inner");
    tree.image("wrap/inner/001.jpg");
    let five = tree.dir("five");
    let leaves = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|name| {
            let leaf = tree.dir(&format!("five/{name}"));
            tree.image(&format!("five/{name}/001.jpg"));
            leaf
        })
        .collect::<Vec<_>>();

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert!(output.pending.iter().any(|entry| entry.path == inner));
    assert!(!output.pending.iter().any(|entry| entry.path == wrap));
    assert!(!output.pending.iter().any(|entry| entry.path == five));
    for leaf in &leaves {
        assert!(
            output.pending.iter().any(|entry| &entry.path == leaf),
            "缺少 {}",
            leaf.display()
        );
    }
    assert_eq!(6, output.summary.pending);
}

#[test]
fn a_subdirectory_holding_another_directory_is_not_a_variant_leaf() {
    let tree = TestTree::new("variant-deep");
    let deep = tree.dir("deep");
    let text = tree.dir("deep/Text");
    tree.image("deep/Text/001.jpg");
    let textless = tree.dir("deep/Textless");
    let sub = tree.dir("deep/Textless/sub");
    tree.image("deep/Textless/sub/001.jpg");

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert!(!output.pending.iter().any(|entry| entry.path == deep));
    assert!(!output.pending.iter().any(|entry| entry.path == textless));
    assert_eq!(2, output.pending.len());
    assert!(output.pending.iter().any(|entry| entry.path == text));
    assert!(output.pending.iter().any(|entry| entry.path == sub));
}

#[test]
fn a_subdirectory_holding_a_zip_is_not_a_variant_leaf() {
    let tree = TestTree::new("variant-zip");
    let mixed = tree.dir("mixed");
    let text = tree.dir("mixed/Text");
    tree.image("mixed/Text/001.jpg");
    tree.image("mixed/Textless/001.jpg");
    let extra = tree.zip("mixed/Textless/extra.zip");

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert!(!output.pending.iter().any(|entry| entry.path == mixed));
    assert_eq!(2, output.pending.len());
    assert!(
        output
            .pending
            .iter()
            .any(|entry| entry.path == text && entry.media_kind == MediaKind::ImageFolder)
    );
    assert!(
        output
            .pending
            .iter()
            .any(|entry| entry.path == extra && entry.media_kind == MediaKind::Zip)
    );
}

#[test]
fn a_parent_with_direct_images_stays_a_single_collection() {
    let tree = TestTree::new("variant-direct-images");
    let direct = tree.dir("direct");
    tree.image("direct/cover.png");
    tree.image("direct/Text/001.jpg");
    tree.image("direct/Textless/001.jpg");

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert_eq!(1, output.pending.len());
    assert_eq!(direct, output.pending[0].path);
    assert_eq!(MediaKind::ImageFolder, output.pending[0].media_kind);
    assert_eq!(1, output.summary.discovered);
    assert_eq!(1, output.summary.pending);
}

#[test]
fn already_indexed_variants_are_never_merged_into_their_parent() {
    let tree = TestTree::new("variant-known-child");
    let legacy = tree.dir("legacy");
    let common = tree.dir("legacy/common");
    tree.image("legacy/common/001.jpg");
    let futa = tree.dir("legacy/futa");
    tree.image("legacy/futa/001.jpg");
    let known = HashMap::from([(common.clone(), MediaKind::ImageFolder)]);

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &known);

    assert!(!output.pending.iter().any(|entry| entry.path == legacy));
    assert!(!output.pending.iter().any(|entry| entry.path == common));
    assert_eq!(1, output.pending.len());
    assert_eq!(futa, output.pending[0].path);
    assert_eq!(2, output.summary.discovered);
    assert_eq!(1, output.summary.skipped_existing);
    assert_eq!(1, output.summary.pending);
}

#[test]
fn a_known_variant_parent_is_skipped_without_descending_into_it() {
    let tree = TestTree::new("variant-known-parent");
    let parent = tree.dir("known-parent");
    tree.image("known-parent/Text/001.jpg");
    tree.image("known-parent/Textless/001.jpg");
    let known = HashMap::from([(parent, MediaKind::ImageFolder)]);

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &known);

    assert!(output.pending.is_empty());
    assert_eq!(0, output.summary.pending);
    assert_eq!(1, output.summary.discovered);
    assert_eq!(1, output.summary.skipped_existing);
    assert!(output.issues.is_empty());
}

#[cfg(windows)]
#[test]
fn a_junction_child_is_not_counted_as_a_variant_subdirectory() {
    let tree = TestTree::new("variant-junction");
    let target = TestTree::new("variant-junction-target");
    let linked = target.dir("Textless");
    target.image("Textless/001.jpg");
    let parent = tree.dir("jl");
    let text = tree.dir("jl/Text");
    tree.image("jl/Text/001.jpg");
    let link = parent.join("Link");
    let created = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link)
        .arg(&linked)
        .status();
    if !created.is_ok_and(|status| status.success()) {
        return;
    }

    let output = scan_new_collections(&[tree.root(SourceKind::Archive)], &HashMap::new());

    assert!(!output.pending.iter().any(|entry| entry.path == parent));
    assert!(!output.pending.iter().any(|entry| entry.path == link));
    assert!(!output.pending.iter().any(|entry| entry.path == linked));
    assert_eq!(1, output.pending.len());
    assert_eq!(text, output.pending[0].path);
}
