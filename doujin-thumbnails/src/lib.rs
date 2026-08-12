//! Bounded cover extraction and WebP thumbnail generation.

use std::cmp::Ordering;
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
use zip::ZipArchive;

const MAX_SOURCE_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 20_000;
const MAX_IMAGE_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DIRECTORY_DEPTH: usize = 32;

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
    let source_image = if source_metadata.is_dir() {
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

fn read_first_zip_image(path: &Path) -> Result<Vec<u8>, ThumbnailError> {
    let file = File::open(path).map_err(source_io)?;
    let mut archive = ZipArchive::new(file).map_err(|error| {
        ThumbnailError::new(
            ThumbnailErrorKind::InvalidArchive,
            format!("無法讀取 ZIP：{error}"),
        )
    })?;
    let mut candidates = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            ThumbnailError::new(
                ThumbnailErrorKind::InvalidArchive,
                format!("無法讀取 ZIP entry：{error}"),
            )
        })?;
        let name = entry.name().replace('\\', "/");
        if !entry.is_dir()
            && !entry.is_symlink()
            && !name.split('/').any(|part| part == "__MACOSX")
            && is_supported_image(Path::new(&name))
        {
            candidates.push((name, index, entry.size()));
        }
    }
    candidates.sort_by(|left, right| natural_cmp(&left.0, &right.0));
    let Some((name, index, size)) = candidates.into_iter().next() else {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::NoSupportedImage,
            "ZIP 內沒有支援的圖片",
        ));
    };
    if size > MAX_SOURCE_IMAGE_BYTES {
        return Err(ThumbnailError::new(
            ThumbnailErrorKind::ResourceLimit,
            format!("封面圖片 {name} 超過 100 MiB 限制"),
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
            format!("封面圖片 {name} 解壓後超過 100 MiB 限制"),
        ));
    }
    Ok(bytes)
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
            })
            .expect_err("reject empty ZIP")
            .kind
        );
    }
}
