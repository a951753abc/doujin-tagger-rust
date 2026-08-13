//! Bounded cover extraction and WebP thumbnail generation.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use doujin_storage::thumbnails::ThumbnailErrorKind;
use image::error::ImageError;
use image::imageops::FilterType;
use image::{DynamicImage, ImageReader, Limits};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

const MAX_SOURCE_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 20_000;
const MAX_IMAGE_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DIRECTORY_DEPTH: usize = 32;
const MAX_SOURCE_ENTRIES: usize = 10_000;
const MAX_CANDIDATE_SCAN_ENTRIES: usize = 96;
const MAX_CANDIDATE_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_COVER_CANDIDATES: usize = 24;
pub const DUPLICATE_FINGERPRINT_ALGORITHM_VERSION: &str = "sha256-pages-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceContentFingerprint {
    pub source_fingerprint: String,
    pub source_size: u64,
    pub file_sha256: Option<String>,
    pub archive_entry_count: usize,
    pub image_count: usize,
    /// SHA-256 of the sorted multiset of page SHA-256 values. Entry names and ZIP
    /// container metadata intentionally do not participate.
    pub content_fingerprint: String,
    /// SHA-256 values in natural page order, used as conservative near-match evidence.
    pub page_hashes: Vec<String>,
    pub perceptual_hashes: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailConfig {
    pub cache_dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub quality: u8,
}

impl ThumbnailConfig {
    pub fn new(
        cache_dir: PathBuf,
        width: u32,
        height: u32,
        quality: u8,
    ) -> Result<Self, ThumbnailError> {
        if !cache_dir.is_absolute() {
            return Err(ThumbnailError::new(
                ThumbnailErrorKind::CacheIo,
                "thumbnail cache 目錄必須是絕對路徑",
            ));
        }
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(ThumbnailError::new(
                ThumbnailErrorKind::ResourceLimit,
                "thumbnail 尺寸必須介於 1 到 4096 像素",
            ));
        }
        if !(1..=100).contains(&quality) {
            return Err(ThumbnailError::new(
                ThumbnailErrorKind::Unsupported,
                "WebP 品質必須介於 1 到 100",
            ));
        }
        Ok(Self {
            cache_dir,
            width,
            height,
            quality,
        })
    }

    pub fn cache_path(&self, collection_id: i64) -> PathBuf {
        self.cache_dir.join(format!("{collection_id}.webp"))
    }

    pub fn settings_fingerprint(&self) -> String {
        format!("{}x{}-q{}-webp-v1", self.width, self.height, self.quality)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailGenerationRequest {
    pub source_path: PathBuf,
    pub cache_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub quality: u8,
    pub selected_entry: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverCandidate {
    pub entry_path: String,
    pub filename: String,
    pub page_order: usize,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailGenerationSuccess {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailError {
    pub kind: ThumbnailErrorKind,
    pub message: String,
}

impl ThumbnailError {
    fn new(kind: ThumbnailErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for ThumbnailError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ThumbnailError {}

pub fn source_fingerprint(source_path: &Path) -> Result<String, ThumbnailError> {
    let metadata = fs::metadata(source_path).map_err(source_io)?;
    let modified = metadata.modified().map_err(source_io)?;
    let duration = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
    let mut fingerprint = format!(
        "{}:{}:{}:{}",
        source_path.to_string_lossy(),
        metadata.len(),
        duration.as_secs(),
        duration.subsec_nanos()
    );
    if metadata.is_dir()
        && let Some(image_path) = first_directory_image(source_path)?
    {
        let image_metadata = fs::metadata(&image_path).map_err(source_io)?;
        let image_modified = image_metadata
            .modified()
            .map_err(source_io)?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        fingerprint.push_str(&format!(
            ":{}:{}:{}:{}",
            image_path.to_string_lossy(),
            image_metadata.len(),
            image_modified.as_secs(),
            image_modified.subsec_nanos()
        ));
    }
    Ok(fingerprint)
}

/// Calculates a versioned duplicate-work fingerprint without unpacking an archive
/// into memory or onto disk. ZIP image entries are streamed through SHA-256 one at
/// a time and retain the thumbnail pipeline's entry-count and per-image limits.
pub fn calculate_source_content_fingerprint(
    source_path: &Path,
) -> Result<SourceContentFingerprint, ThumbnailError> {
    let metadata = fs::symlink_metadata(source_path).map_err(source_io)?;
    if metadata.file_type().is_symlink() {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::Unsupported,
            "收藏來源不可為 symbolic link",
        ));
    }
    if metadata.is_file() {
        calculate_zip_content_fingerprint(source_path)
    } else if metadata.is_dir() {
        calculate_directory_content_fingerprint(source_path)
    } else {
        Err(ThumbnailError::new(
            ThumbnailErrorKind::Unsupported,
            "收藏來源不是 ZIP 檔案或圖片資料夾",
        ))
    }
}

/// Cheap, strong-enough cache identity used before deciding whether full content
/// hashing is necessary. Directory identity covers every supported image's path,
/// size, and mtime; archive identity covers the ZIP path, size, and mtime.
pub fn duplicate_source_fingerprint(source_path: &Path) -> Result<String, ThumbnailError> {
    let metadata = fs::symlink_metadata(source_path).map_err(source_io)?;
    if metadata.file_type().is_symlink() {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::Unsupported,
            "收藏來源不可為 symbolic link",
        ));
    }
    if metadata.is_dir() {
        Ok(directory_source_identity(source_path)?.0)
    } else if metadata.is_file() {
        source_fingerprint(source_path)
    } else {
        Err(ThumbnailError::new(
            ThumbnailErrorKind::Unsupported,
            "收藏來源不是 ZIP 檔案或圖片資料夾",
        ))
    }
}

fn calculate_zip_content_fingerprint(
    source_path: &Path,
) -> Result<SourceContentFingerprint, ThumbnailError> {
    let before = source_fingerprint(source_path)?;
    let source_size = fs::metadata(source_path).map_err(source_io)?.len();
    let file_sha256 = hash_file(source_path)?;
    let file = File::open(source_path).map_err(source_io)?;
    let mut archive = ZipArchive::new(file).map_err(|error| {
        ThumbnailError::new(
            ThumbnailErrorKind::InvalidArchive,
            format!("無法讀取 ZIP：{error}"),
        )
    })?;
    let archive_entry_count = archive.len();
    if archive_entry_count > MAX_SOURCE_ENTRIES {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::ResourceLimit,
            format!("ZIP entry 數超過 {MAX_SOURCE_ENTRIES} 筆限制"),
        ));
    }

    let mut identities = HashMap::new();
    let mut pages = Vec::new();
    for index in 0..archive_entry_count {
        let mut entry = archive.by_index(index).map_err(|error| {
            ThumbnailError::new(
                ThumbnailErrorKind::InvalidArchive,
                format!("無法讀取 ZIP entry #{index}：{error}"),
            )
        })?;
        if entry.is_dir() || entry.is_symlink() {
            continue;
        }
        let raw_name = entry.name().to_owned();
        if !is_supported_image(Path::new(&raw_name)) {
            continue;
        }
        let name = normalize_entry_path(&raw_name).ok_or_else(|| {
            ThumbnailError::new(
                ThumbnailErrorKind::InvalidArchive,
                format!("ZIP 圖片 entry path 不安全：{raw_name}"),
            )
        })?;
        if name.split('/').any(|part| part == "__MACOSX") {
            continue;
        }
        if identities.insert(name.clone(), index).is_some() {
            return Err(ThumbnailError::new(
                ThumbnailErrorKind::InvalidArchive,
                format!("ZIP 包含重複的圖片 entry identity：{name}"),
            ));
        }
        if entry.size() > MAX_SOURCE_IMAGE_BYTES {
            return Err(ThumbnailError::new(
                ThumbnailErrorKind::ResourceLimit,
                format!("ZIP 圖片 {name} 解壓後超過 100 MiB 限制"),
            ));
        }
        let hash = hash_bounded_reader(&mut entry, MAX_SOURCE_IMAGE_BYTES, &name)?;
        pages.push((name, hash));
    }
    if pages.is_empty() {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::NoSupportedImage,
            "ZIP 內沒有支援的圖片",
        ));
    }
    pages.sort_by(|left, right| natural_cmp(&left.0, &right.0));
    let page_hashes = pages.into_iter().map(|(_, hash)| hash).collect::<Vec<_>>();
    let after = source_fingerprint(source_path)?;
    if before != after {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::SourceIo,
            "收藏來源在 fingerprint 計算期間發生變更，請重試",
        ));
    }
    Ok(SourceContentFingerprint {
        source_fingerprint: after,
        source_size,
        file_sha256: Some(file_sha256),
        archive_entry_count,
        image_count: page_hashes.len(),
        content_fingerprint: combine_page_hashes(&page_hashes),
        page_hashes,
        perceptual_hashes: None,
    })
}

fn calculate_directory_content_fingerprint(
    source_path: &Path,
) -> Result<SourceContentFingerprint, ThumbnailError> {
    let (before, source_size, paths) = directory_source_identity(source_path)?;
    if paths.is_empty() {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::NoSupportedImage,
            "圖片資料夾內沒有支援的圖片",
        ));
    }
    let mut pages = Vec::with_capacity(paths.len());
    for path in &paths {
        let relative = path
            .strip_prefix(source_path)
            .map_err(|_| {
                ThumbnailError::new(
                    ThumbnailErrorKind::Unsupported,
                    format!("圖片不在收藏來源內：{}", path.display()),
                )
            })?
            .to_string_lossy()
            .replace('\\', "/");
        let size = fs::metadata(path).map_err(source_io)?.len();
        if size > MAX_SOURCE_IMAGE_BYTES {
            return Err(ThumbnailError::new(
                ThumbnailErrorKind::ResourceLimit,
                format!("圖片 {relative} 超過 100 MiB 限制"),
            ));
        }
        pages.push((relative, hash_file(path)?));
    }
    pages.sort_by(|left, right| natural_cmp(&left.0, &right.0));
    let page_hashes = pages.into_iter().map(|(_, hash)| hash).collect::<Vec<_>>();
    let (after, after_size, _) = directory_source_identity(source_path)?;
    if before != after || source_size != after_size {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::SourceIo,
            "收藏來源在 fingerprint 計算期間發生變更，請重試",
        ));
    }
    Ok(SourceContentFingerprint {
        source_fingerprint: after,
        source_size,
        file_sha256: None,
        archive_entry_count: page_hashes.len(),
        image_count: page_hashes.len(),
        content_fingerprint: combine_page_hashes(&page_hashes),
        page_hashes,
        perceptual_hashes: None,
    })
}

fn directory_source_identity(
    source_path: &Path,
) -> Result<(String, u64, Vec<PathBuf>), ThumbnailError> {
    let mut paths = Vec::new();
    collect_directory_images(source_path, source_path, 0, &mut paths)?;
    paths.sort_by(|left, right| {
        natural_cmp(
            &left
                .strip_prefix(source_path)
                .unwrap_or(left)
                .to_string_lossy(),
            &right
                .strip_prefix(source_path)
                .unwrap_or(right)
                .to_string_lossy(),
        )
    });
    let mut identity = Sha256::new();
    identity.update(b"doujin-directory-source-v1\0");
    let mut total_size = 0u64;
    for path in &paths {
        let relative = path.strip_prefix(source_path).map_err(|_| {
            ThumbnailError::new(
                ThumbnailErrorKind::Unsupported,
                format!("圖片不在收藏來源內：{}", path.display()),
            )
        })?;
        let metadata = fs::metadata(path).map_err(source_io)?;
        let modified = metadata
            .modified()
            .map_err(source_io)?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let name = relative.to_string_lossy().replace('\\', "/");
        update_framed(&mut identity, name.as_bytes());
        identity.update(metadata.len().to_le_bytes());
        identity.update(modified.as_secs().to_le_bytes());
        identity.update(modified.subsec_nanos().to_le_bytes());
        total_size = total_size.saturating_add(metadata.len());
    }
    Ok((
        format!("directory-v1:{}", hex_digest(identity.finalize())),
        total_size,
        paths,
    ))
}

fn hash_file(path: &Path) -> Result<String, ThumbnailError> {
    let mut file = File::open(path).map_err(source_io)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(source_io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize()))
}

fn hash_bounded_reader(
    reader: &mut impl Read,
    limit: u64,
    entry_name: &str,
) -> Result<String, ThumbnailError> {
    let mut reader = reader.take(limit + 1);
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(zip_entry_io)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > limit {
            return Err(ThumbnailError::new(
                ThumbnailErrorKind::ResourceLimit,
                format!("ZIP 圖片 {entry_name} 解壓後超過 100 MiB 限制"),
            ));
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize()))
}

fn combine_page_hashes(page_hashes: &[String]) -> String {
    let mut sorted = page_hashes.to_vec();
    sorted.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(b"doujin-page-multiset-v1\0");
    hasher.update((sorted.len() as u64).to_le_bytes());
    for page_hash in sorted {
        update_framed(&mut hasher, page_hash.as_bytes());
    }
    hex_digest(hasher.finalize())
}

fn update_framed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub fn cover_source_fingerprint(
    source_path: &Path,
    selected_entry: Option<&str>,
) -> Result<String, ThumbnailError> {
    let mut fingerprint = source_fingerprint(source_path)?;
    match selected_entry {
        None => fingerprint.push_str(":cover:auto"),
        Some(entry_path) => {
            let normalized = normalize_entry_path(entry_path).ok_or_else(|| {
                ThumbnailError::new(
                    ThumbnailErrorKind::Unsupported,
                    "指定封面 entry path 不安全",
                )
            })?;
            fingerprint.push_str(&format!(":cover:manual:{}:{normalized}", normalized.len()));
            if source_path.is_dir() {
                let image_path = find_directory_entry(source_path, &normalized)?;
                let metadata = fs::metadata(image_path).map_err(source_io)?;
                let modified = metadata
                    .modified()
                    .map_err(source_io)?
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                fingerprint.push_str(&format!(
                    ":{}:{}:{}",
                    metadata.len(),
                    modified.as_secs(),
                    modified.subsec_nanos()
                ));
            }
        }
    }
    Ok(fingerprint)
}

pub fn cover_candidates(
    source_path: &Path,
    limit: usize,
) -> Result<Vec<CoverCandidate>, ThumbnailError> {
    let limit = limit.clamp(1, MAX_COVER_CANDIDATES);
    let entries = source_entries(source_path)?;
    let mut candidates = Vec::with_capacity(limit);
    let mut total_bytes = 0u64;
    for (page_order, entry_path) in entries
        .into_iter()
        .take(MAX_CANDIDATE_SCAN_ENTRIES)
        .enumerate()
    {
        if candidates.len() == limit {
            break;
        }
        let Ok(bytes) = read_source_entry(source_path, &entry_path) else {
            continue;
        };
        total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        if total_bytes > MAX_CANDIDATE_TOTAL_BYTES {
            return Err(ThumbnailError::new(
                ThumbnailErrorKind::ResourceLimit,
                "候選封面累計解壓資料超過 256 MiB 限制",
            ));
        }
        let Ok(image) = decode_image(bytes) else {
            continue;
        };
        candidates.push(CoverCandidate {
            filename: entry_path
                .rsplit('/')
                .next()
                .unwrap_or(&entry_path)
                .to_owned(),
            entry_path,
            page_order: page_order + 1,
            width: image.width(),
            height: image.height(),
        });
    }
    Ok(candidates)
}

pub fn validate_cover_candidate(
    source_path: &Path,
    entry_path: &str,
) -> Result<CoverCandidate, ThumbnailError> {
    let normalized = normalize_entry_path(entry_path).ok_or_else(|| {
        ThumbnailError::new(
            ThumbnailErrorKind::Unsupported,
            "指定封面 entry path 不安全",
        )
    })?;
    let entries = source_entries(source_path)?;
    let page_order = entries
        .iter()
        .position(|entry| entry == &normalized)
        .ok_or_else(|| {
            ThumbnailError::new(
                ThumbnailErrorKind::NoSupportedImage,
                format!("指定封面 entry 已不存在：{normalized}"),
            )
        })?;
    let image = read_source_entry(source_path, &normalized).and_then(decode_image)?;
    Ok(CoverCandidate {
        filename: normalized
            .rsplit('/')
            .next()
            .unwrap_or(&normalized)
            .to_owned(),
        entry_path: normalized,
        page_order: page_order + 1,
        width: image.width(),
        height: image.height(),
    })
}

pub fn cover_candidate_preview(
    source_path: &Path,
    entry_path: &str,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, ThumbnailError> {
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::ResourceLimit,
            "候選封面預覽尺寸必須介於 1 到 1024 像素",
        ));
    }
    let normalized = validate_cover_candidate(source_path, entry_path)?.entry_path;
    let image = read_source_entry(source_path, &normalized).and_then(decode_image)?;
    let resized =
        DynamicImage::ImageRgba8(image.resize(width, height, FilterType::Lanczos3).to_rgba8());
    Ok(webp::Encoder::from_image(&resized)
        .map_err(|message| ThumbnailError::new(ThumbnailErrorKind::Unsupported, message))?
        .encode(76.0)
        .to_vec())
}

pub fn generate_thumbnail(
    request: &ThumbnailGenerationRequest,
) -> Result<ThumbnailGenerationSuccess, ThumbnailError> {
    if request.width == 0 || request.height == 0 || request.quality == 0 || request.quality > 100 {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::Unsupported,
            "thumbnail 生成參數無效",
        ));
    }
    let source_metadata = fs::metadata(&request.source_path).map_err(source_io)?;
    let source_image = if let Some(entry_path) = request.selected_entry.as_deref() {
        read_source_entry(&request.source_path, entry_path)?
    } else if source_metadata.is_dir() {
        read_first_directory_image(&request.source_path)?
    } else if source_metadata.is_file() {
        read_first_zip_image(&request.source_path)?
    } else {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::Unsupported,
            "收藏來源不是 ZIP 檔案或圖片資料夾",
        ));
    };
    let image = decode_image(source_image)?;
    let resized = image.resize(request.width, request.height, FilterType::Lanczos3);
    let resized = DynamicImage::ImageRgba8(resized.to_rgba8());
    let encoded = webp::Encoder::from_image(&resized)
        .map_err(|message| ThumbnailError::new(ThumbnailErrorKind::Unsupported, message))?
        .encode(request.quality.into());
    publish_cache(&request.cache_path, encoded.as_ref())?;
    Ok(ThumbnailGenerationSuccess {
        width: resized.width(),
        height: resized.height(),
    })
}

pub fn transparent_placeholder_webp() -> &'static [u8] {
    static PLACEHOLDER: OnceLock<Vec<u8>> = OnceLock::new();
    PLACEHOLDER
        .get_or_init(|| {
            webp::Encoder::from_rgba(&[0, 0, 0, 0], 1, 1)
                .encode_lossless()
                .to_vec()
        })
        .as_slice()
}

fn normalize_entry_path(value: &str) -> Option<String> {
    let value = value.replace('\\', "/");
    if value.is_empty() || value.starts_with('/') || value.contains('\0') {
        return None;
    }
    let mut normalized = Vec::new();
    for component in value.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." || component.contains(':') {
            return None;
        }
        normalized.push(component);
    }
    (!normalized.is_empty()).then(|| normalized.join("/"))
}

fn source_entries(source_path: &Path) -> Result<Vec<String>, ThumbnailError> {
    if source_path.is_dir() {
        let mut paths = Vec::new();
        collect_directory_images(source_path, source_path, 0, &mut paths)?;
        let mut entries = paths
            .into_iter()
            .filter_map(|path| {
                path.strip_prefix(source_path)
                    .ok()
                    .and_then(|relative| normalize_entry_path(&relative.to_string_lossy()))
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| natural_cmp(left, right));
        return Ok(entries);
    }
    let file = File::open(source_path).map_err(source_io)?;
    let mut archive = ZipArchive::new(file).map_err(|error| {
        ThumbnailError::new(
            ThumbnailErrorKind::InvalidArchive,
            format!("無法讀取 ZIP：{error}"),
        )
    })?;
    if archive.len() > MAX_SOURCE_ENTRIES {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::ResourceLimit,
            format!("ZIP entry 數超過 {MAX_SOURCE_ENTRIES} 筆限制"),
        ));
    }
    let mut entries = Vec::new();
    let mut counts = HashMap::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            ThumbnailError::new(
                ThumbnailErrorKind::InvalidArchive,
                format!("無法讀取 ZIP entry：{error}"),
            )
        })?;
        let Some(name) = normalize_entry_path(entry.name()) else {
            continue;
        };
        if entry.is_dir()
            || entry.is_symlink()
            || name.split('/').any(|part| part == "__MACOSX")
            || !is_supported_image(Path::new(&name))
        {
            continue;
        }
        *counts.entry(name.clone()).or_insert(0usize) += 1;
        entries.push(name);
    }
    entries.retain(|name| counts.get(name) == Some(&1));
    entries.sort_by(|left, right| natural_cmp(left, right));
    Ok(entries)
}

fn read_source_entry(source_path: &Path, entry_path: &str) -> Result<Vec<u8>, ThumbnailError> {
    let normalized = normalize_entry_path(entry_path).ok_or_else(|| {
        ThumbnailError::new(
            ThumbnailErrorKind::Unsupported,
            "指定封面 entry path 不安全",
        )
    })?;
    if source_path.is_dir() {
        let path = find_directory_entry(source_path, &normalized)?;
        let metadata = fs::metadata(&path).map_err(source_io)?;
        if metadata.len() > MAX_SOURCE_IMAGE_BYTES {
            return Err(ThumbnailError::new(
                ThumbnailErrorKind::ResourceLimit,
                format!("封面圖片 {normalized} 超過 100 MiB 限制"),
            ));
        }
        return fs::read(path).map_err(source_io);
    }
    let file = File::open(source_path).map_err(source_io)?;
    let mut archive = ZipArchive::new(file).map_err(|error| {
        ThumbnailError::new(
            ThumbnailErrorKind::InvalidArchive,
            format!("無法讀取 ZIP：{error}"),
        )
    })?;
    if archive.len() > MAX_SOURCE_ENTRIES {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::ResourceLimit,
            format!("ZIP entry 數超過 {MAX_SOURCE_ENTRIES} 筆限制"),
        ));
    }
    let mut matched_index = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            ThumbnailError::new(
                ThumbnailErrorKind::InvalidArchive,
                format!("無法讀取 ZIP entry：{error}"),
            )
        })?;
        if !entry.is_dir()
            && !entry.is_symlink()
            && normalize_entry_path(entry.name()).as_deref() == Some(&normalized)
        {
            if matched_index.is_some() {
                return Err(ThumbnailError::new(
                    ThumbnailErrorKind::InvalidArchive,
                    format!("ZIP 包含重複的封面 entry identity：{normalized}"),
                ));
            }
            matched_index = Some((index, entry.size()));
        }
    }
    let Some((index, size)) = matched_index else {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::NoSupportedImage,
            format!("指定封面 entry 已不存在：{normalized}"),
        ));
    };
    if size > MAX_SOURCE_IMAGE_BYTES {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::ResourceLimit,
            format!("封面圖片 {normalized} 超過 100 MiB 限制"),
        ));
    }
    let entry = archive.by_index(index).map_err(|error| {
        ThumbnailError::new(
            ThumbnailErrorKind::InvalidArchive,
            format!("無法重新開啟 ZIP entry：{error}"),
        )
    })?;
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    entry
        .take(MAX_SOURCE_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(zip_entry_io)?;
    if bytes.len() as u64 > MAX_SOURCE_IMAGE_BYTES {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::ResourceLimit,
            format!("封面圖片 {normalized} 解壓後超過 100 MiB 限制"),
        ));
    }
    Ok(bytes)
}

fn find_directory_entry(root: &Path, entry_path: &str) -> Result<PathBuf, ThumbnailError> {
    let entries = source_entries(root)?;
    if !entries.iter().any(|entry| entry == entry_path) {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::NoSupportedImage,
            format!("指定封面 entry 已不存在：{entry_path}"),
        ));
    }
    Ok(entry_path
        .split('/')
        .fold(root.to_path_buf(), |path, component| path.join(component)))
}

fn read_first_zip_image(path: &Path) -> Result<Vec<u8>, ThumbnailError> {
    let Some(entry_path) = source_entries(path)?.into_iter().next() else {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::NoSupportedImage,
            "ZIP 內沒有支援的圖片",
        ));
    };
    read_source_entry(path, &entry_path)
}

fn read_first_directory_image(path: &Path) -> Result<Vec<u8>, ThumbnailError> {
    let image_path = first_directory_image(path)?.ok_or_else(|| {
        ThumbnailError::new(
            ThumbnailErrorKind::NoSupportedImage,
            "圖片資料夾內沒有支援的圖片",
        )
    })?;
    let metadata = fs::metadata(&image_path).map_err(source_io)?;
    if metadata.len() > MAX_SOURCE_IMAGE_BYTES {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::ResourceLimit,
            format!("封面圖片 {} 超過 100 MiB 限制", image_path.display()),
        ));
    }
    fs::read(&image_path).map_err(source_io)
}

fn first_directory_image(path: &Path) -> Result<Option<PathBuf>, ThumbnailError> {
    let mut candidates = Vec::new();
    collect_directory_images(path, path, 0, &mut candidates)?;
    candidates.sort_by(|left, right| {
        natural_cmp(
            &left.strip_prefix(path).unwrap_or(left).to_string_lossy(),
            &right.strip_prefix(path).unwrap_or(right).to_string_lossy(),
        )
    });
    Ok(candidates.into_iter().next())
}

fn collect_directory_images(
    root: &Path,
    directory: &Path,
    depth: usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), ThumbnailError> {
    if depth > MAX_DIRECTORY_DEPTH {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::ResourceLimit,
            format!(
                "圖片資料夾層級超過 {MAX_DIRECTORY_DEPTH} 層：{}",
                root.display()
            ),
        ));
    }
    for entry in fs::read_dir(directory).map_err(source_io)? {
        let entry = entry.map_err(source_io)?;
        let file_type = entry.file_type().map_err(source_io)?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            if entry.file_name() != "__MACOSX" {
                collect_directory_images(root, &path, depth + 1, output)?;
            }
        } else if file_type.is_file() && is_supported_image(&path) {
            output.push(path);
            if output.len() > MAX_SOURCE_ENTRIES {
                return Err(ThumbnailError::new(
                    ThumbnailErrorKind::ResourceLimit,
                    format!("圖片 entry 數超過 {MAX_SOURCE_ENTRIES} 筆限制"),
                ));
            }
        }
    }
    Ok(())
}

fn decode_image(bytes: Vec<u8>) -> Result<DynamicImage, ThumbnailError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(source_io)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC_BYTES);
    reader.limits(limits);
    reader.decode().map_err(|error| match error {
        ImageError::Limits(_) => ThumbnailError::new(
            ThumbnailErrorKind::ResourceLimit,
            format!("圖片超過解碼資源限制：{error}"),
        ),
        ImageError::Unsupported(_) => ThumbnailError::new(
            ThumbnailErrorKind::Unsupported,
            format!("不支援的圖片格式：{error}"),
        ),
        _ => ThumbnailError::new(
            ThumbnailErrorKind::ImageDecode,
            format!("圖片解碼失敗：{error}"),
        ),
    })
}

fn publish_cache(path: &Path, bytes: &[u8]) -> Result<(), ThumbnailError> {
    let parent = path.parent().ok_or_else(|| {
        ThumbnailError::new(
            ThumbnailErrorKind::CacheIo,
            "thumbnail cache 路徑沒有父目錄",
        )
    })?;
    fs::create_dir_all(parent).map_err(cache_io)?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".thumbnail-{}-{unique}.part", std::process::id()));
    let publish = (|| -> Result<(), ThumbnailError> {
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(cache_io)?;
        file.write_all(bytes).map_err(cache_io)?;
        file.sync_all().map_err(cache_io)?;
        if path.exists() {
            fs::remove_file(path).map_err(cache_io)?;
        }
        fs::rename(&temporary, path).map_err(cache_io)
    })();
    if publish.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    publish
}

fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp"
            )
        })
}

fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        if left[left_index].is_ascii_digit() && right[right_index].is_ascii_digit() {
            let left_end = digit_end(left, left_index);
            let right_end = digit_end(right, right_index);
            let left_digits = trim_zeroes(&left[left_index..left_end]);
            let right_digits = trim_zeroes(&right[right_index..right_end]);
            let numeric = left_digits
                .len()
                .cmp(&right_digits.len())
                .then_with(|| left_digits.cmp(right_digits));
            if numeric != Ordering::Equal {
                return numeric;
            }
            left_index = left_end;
            right_index = right_end;
        } else {
            let ordering = left[left_index]
                .to_ascii_lowercase()
                .cmp(&right[right_index].to_ascii_lowercase());
            if ordering != Ordering::Equal {
                return ordering;
            }
            left_index += 1;
            right_index += 1;
        }
    }
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn digit_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    end
}

fn trim_zeroes(digits: &[u8]) -> &[u8] {
    let first_nonzero = digits
        .iter()
        .position(|digit| *digit != b'0')
        .unwrap_or(digits.len().saturating_sub(1));
    &digits[first_nonzero..]
}

fn source_io(error: std::io::Error) -> ThumbnailError {
    ThumbnailError::new(
        ThumbnailErrorKind::SourceIo,
        format!("讀取收藏失敗：{error}"),
    )
}

fn cache_io(error: std::io::Error) -> ThumbnailError {
    ThumbnailError::new(
        ThumbnailErrorKind::CacheIo,
        format!("寫入縮圖快取失敗：{error}"),
    )
}

fn zip_entry_io(error: std::io::Error) -> ThumbnailError {
    ThumbnailError::new(
        ThumbnailErrorKind::InvalidArchive,
        format!("ZIP 圖片資料損壞或無法解壓：{error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, ImageFormat, Rgba};
    use std::io::Seek;
    use zip::write::SimpleFileOptions;

    struct TestTree(PathBuf);

    impl TestTree {
        fn new(label: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "doujin-thumbnails-{label}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test tree");
            Self(path)
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            if self
                .0
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("doujin-thumbnails-"))
            {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }

    fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(width, height, Rgba(color)));
        let mut output = Cursor::new(Vec::new());
        image
            .write_to(&mut output, ImageFormat::Png)
            .expect("encode PNG");
        output.into_inner()
    }

    fn zip_with_images<W: Write + Seek>(writer: W, images: &[(&str, Vec<u8>)]) {
        let mut archive = zip::ZipWriter::new(writer);
        for (name, bytes) in images {
            archive
                .start_file(*name, SimpleFileOptions::default())
                .expect("start ZIP image");
            archive.write_all(bytes).expect("write ZIP image");
        }
        archive.finish().expect("finish ZIP");
    }

    #[test]
    fn zip_generation_uses_naturally_first_image_and_fits_bounds() {
        let tree = TestTree::new("zip");
        let source = tree.0.join("book.zip");
        zip_with_images(
            File::create(&source).expect("create ZIP"),
            &[
                ("page10.png", png(100, 200, [0, 0, 255, 255])),
                ("page2.png", png(600, 300, [255, 0, 0, 255])),
            ],
        );
        let cache = tree.0.join("cache").join("1.webp");

        let generated = generate_thumbnail(&ThumbnailGenerationRequest {
            source_path: source,
            cache_path: cache.clone(),
            width: 300,
            height: 400,
            quality: 80,
            selected_entry: None,
        })
        .expect("generate thumbnail");

        assert_eq!((300, 150), (generated.width, generated.height));
        let decoded = image::open(&cache).expect("decode WebP cache");
        assert_eq!((300, 150), (decoded.width(), decoded.height()));
        let pixel = decoded.to_rgb8().get_pixel(150, 75).0;
        assert!(pixel[0] > pixel[2], "page2 red cover should be selected");
    }

    #[test]
    fn directory_generation_skips_symlinks_and_uses_natural_order() {
        let tree = TestTree::new("directory");
        let source = tree.0.join("images");
        fs::create_dir(&source).expect("create image directory");
        fs::write(source.join("10.png"), png(20, 40, [0, 0, 255, 255])).expect("page 10");
        fs::write(source.join("2.png"), png(40, 20, [255, 0, 0, 255])).expect("page 2");
        let cache = tree.0.join("cache").join("2.webp");

        let generated = generate_thumbnail(&ThumbnailGenerationRequest {
            source_path: source,
            cache_path: cache,
            width: 100,
            height: 100,
            quality: 80,
            selected_entry: None,
        })
        .expect("generate directory thumbnail");

        assert_eq!((100, 50), (generated.width, generated.height));
    }

    #[test]
    fn invalid_archive_and_empty_archive_are_permanent_failures() {
        let tree = TestTree::new("errors");
        let invalid = tree.0.join("invalid.zip");
        fs::write(&invalid, b"not a zip").expect("invalid ZIP");
        let request = ThumbnailGenerationRequest {
            source_path: invalid,
            cache_path: tree.0.join("invalid.webp"),
            width: 100,
            height: 100,
            quality: 80,
            selected_entry: None,
        };
        assert_eq!(
            ThumbnailErrorKind::InvalidArchive,
            generate_thumbnail(&request)
                .expect_err("reject invalid ZIP")
                .kind
        );

        let empty = tree.0.join("empty.zip");
        zip_with_images(File::create(&empty).expect("create empty ZIP"), &[]);
        assert_eq!(
            ThumbnailErrorKind::NoSupportedImage,
            generate_thumbnail(&ThumbnailGenerationRequest {
                source_path: empty,
                cache_path: tree.0.join("empty.webp"),
                width: 100,
                height: 100,
                quality: 80,
                selected_entry: None,
            })
            .expect_err("reject empty ZIP")
            .kind
        );
    }

    #[test]
    fn candidates_are_safe_decodable_naturally_ordered_and_selected_generation_is_exact() {
        let tree = TestTree::new("cover-candidates");
        let source = tree.0.join("book.zip");
        zip_with_images(
            File::create(&source).expect("create ZIP"),
            &[
                ("../escape.png", png(20, 20, [0, 255, 0, 255])),
                ("notes.txt", b"not an image".to_vec()),
                ("page10.png", png(20, 40, [0, 0, 255, 255])),
                ("page2.png", png(40, 20, [255, 0, 0, 255])),
                ("broken.png", b"not a png".to_vec()),
            ],
        );

        let candidates = cover_candidates(&source, MAX_COVER_CANDIDATES).expect("candidates");
        assert_eq!(
            vec!["page2.png", "page10.png"],
            candidates
                .iter()
                .map(|candidate| candidate.entry_path.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            vec![2, 3],
            candidates
                .iter()
                .map(|item| item.page_order)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            ThumbnailErrorKind::Unsupported,
            validate_cover_candidate(&source, "../escape.png")
                .expect_err("reject traversal")
                .kind
        );

        let cache = tree.0.join("selected.webp");
        generate_thumbnail(&ThumbnailGenerationRequest {
            source_path: source.clone(),
            cache_path: cache.clone(),
            width: 100,
            height: 100,
            quality: 80,
            selected_entry: Some("page10.png".to_owned()),
        })
        .expect("generate selected page");
        let pixel = image::open(cache)
            .expect("decode selected cache")
            .to_rgb8()
            .get_pixel(25, 50)
            .0;
        assert!(pixel[2] > pixel[0], "page10 blue image must be used");
        assert_eq!(
            ThumbnailErrorKind::NoSupportedImage,
            validate_cover_candidate(&source, "missing.png")
                .expect_err("missing override")
                .kind
        );
    }

    #[test]
    fn candidates_enforce_limit_and_preview_is_bounded() {
        let tree = TestTree::new("candidate-limits");
        let source = tree.0.join("book.zip");
        let images = (0..30)
            .map(|index| (format!("{index:02}.png"), png(8, 8, [index, 0, 0, 255])))
            .collect::<Vec<_>>();
        let references = images
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.clone()))
            .collect::<Vec<_>>();
        zip_with_images(File::create(&source).expect("create ZIP"), &references);

        assert_eq!(
            MAX_COVER_CANDIDATES,
            cover_candidates(&source, usize::MAX)
                .expect("limited candidates")
                .len()
        );
        let preview =
            cover_candidate_preview(&source, "00.png", 160, 220).expect("bounded preview");
        let decoded = image::load_from_memory(&preview).expect("decode preview");
        assert!(decoded.width() <= 160 && decoded.height() <= 220);
        assert_eq!(
            ThumbnailErrorKind::ResourceLimit,
            cover_candidate_preview(&source, "00.png", 2048, 220)
                .expect_err("reject oversized preview")
                .kind
        );

        let oversized_source = tree.0.join("oversized-dimension.zip");
        zip_with_images(
            File::create(&oversized_source).expect("create oversized ZIP"),
            &[("huge.png", png(MAX_IMAGE_DIMENSION + 1, 1, [0, 0, 0, 255]))],
        );
        assert_eq!(
            ThumbnailErrorKind::ResourceLimit,
            validate_cover_candidate(&oversized_source, "huge.png")
                .expect_err("reject oversized decoded dimensions")
                .kind
        );
        assert!(
            cover_candidates(&oversized_source, MAX_COVER_CANDIDATES)
                .expect("skip resource-limited candidate")
                .is_empty()
        );
    }

    #[test]
    fn duplicate_fingerprint_distinguishes_exact_file_from_same_page_content() {
        let tree = TestTree::new("duplicate-fingerprint");
        let first = tree.0.join("first.zip");
        let exact_copy = tree.0.join("renamed-copy.zip");
        let repacked = tree.0.join("repacked.zip");
        let first_page = png(16, 24, [190, 30, 20, 255]);
        let second_page = png(16, 24, [20, 30, 190, 255]);
        zip_with_images(
            File::create(&first).expect("create first ZIP"),
            &[
                ("page-01.png", first_page.clone()),
                ("page-02.png", second_page.clone()),
            ],
        );
        fs::copy(&first, &exact_copy).expect("copy exact ZIP");
        zip_with_images(
            File::create(&repacked).expect("create repacked ZIP"),
            &[
                ("renamed/002.png", second_page),
                ("renamed/001.png", first_page),
            ],
        );

        let first = calculate_source_content_fingerprint(&first).expect("first fingerprint");
        let exact = calculate_source_content_fingerprint(&exact_copy).expect("exact fingerprint");
        let repacked =
            calculate_source_content_fingerprint(&repacked).expect("repacked fingerprint");
        assert_eq!(first.file_sha256, exact.file_sha256);
        assert_eq!(first.content_fingerprint, exact.content_fingerprint);
        assert_ne!(first.source_fingerprint, exact.source_fingerprint);
        assert_ne!(first.file_sha256, repacked.file_sha256);
        assert_eq!(first.content_fingerprint, repacked.content_fingerprint);
        assert_eq!(first.page_hashes, repacked.page_hashes);
        assert_eq!(2, repacked.image_count);
        assert_eq!(2, repacked.archive_entry_count);
    }

    #[test]
    fn duplicate_fingerprint_streams_directories_and_rejects_ambiguous_zip_entries() {
        let tree = TestTree::new("duplicate-fingerprint-safety");
        let directory = tree.0.join("pages");
        fs::create_dir(&directory).expect("create pages directory");
        fs::write(directory.join("01.png"), png(8, 8, [1, 2, 3, 255])).expect("page one");
        fs::write(directory.join("02.png"), png(8, 8, [4, 5, 6, 255])).expect("page two");
        let fingerprint =
            calculate_source_content_fingerprint(&directory).expect("directory fingerprint");
        assert_eq!(None, fingerprint.file_sha256);
        assert_eq!(2, fingerprint.image_count);
        assert!(fingerprint.source_fingerprint.starts_with("directory-v1:"));

        let ambiguous = tree.0.join("ambiguous.zip");
        zip_with_images(
            File::create(&ambiguous).expect("create ambiguous ZIP"),
            &[
                ("pages/01.png", png(8, 8, [1, 2, 3, 255])),
                ("pages\\01.png", png(8, 8, [4, 5, 6, 255])),
            ],
        );
        assert_eq!(
            ThumbnailErrorKind::InvalidArchive,
            calculate_source_content_fingerprint(&ambiguous)
                .expect_err("reject duplicate normalized identity")
                .kind
        );
    }
}
