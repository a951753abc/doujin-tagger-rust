use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::domain::{ParodyEvidence, ParseInput, ParseStatus};
use crate::parser::parse_filename;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PercentDecodeError {
    InvalidEscape { index: usize },
    InvalidUtf8,
}

impl fmt::Display for PercentDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEscape { index } => {
                write!(formatter, "invalid percent escape at byte {index}")
            }
            Self::InvalidUtf8 => write!(formatter, "percent-decoded filename is not valid UTF-8"),
        }
    }
}

impl Error for PercentDecodeError {}

pub fn decode_percent_encoded_filename(
    filename: &str,
) -> Result<Option<String>, PercentDecodeError> {
    if !filename.as_bytes().contains(&b'%') {
        return Ok(None);
    }

    let input = filename.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    let mut decoded_any = false;

    while index < input.len() {
        if input[index] != b'%' {
            output.push(input[index]);
            index += 1;
            continue;
        }

        let high = input
            .get(index + 1)
            .and_then(|byte| hex_value(*byte))
            .ok_or(PercentDecodeError::InvalidEscape { index })?;
        let low = input
            .get(index + 2)
            .and_then(|byte| hex_value(*byte))
            .ok_or(PercentDecodeError::InvalidEscape { index })?;
        output.push((high << 4) | low);
        decoded_any = true;
        index += 3;
    }

    let decoded = String::from_utf8(output).map_err(|_| PercentDecodeError::InvalidUtf8)?;
    Ok(decoded_any.then_some(decoded))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameOutcome {
    NotPercentEncoded,
    NotStructurallyParsed { decoded_filename: String },
    Renamed { original: PathBuf, renamed: PathBuf },
}

#[derive(Debug)]
pub enum RenameError {
    MissingFilename,
    NonUnicodeFilename,
    PercentDecode(PercentDecodeError),
    UnsafeDecodedFilename(String),
    TargetExists(PathBuf),
    Filesystem(io::Error),
    SourceRemoval {
        source: io::Error,
        rollback: Option<io::Error>,
    },
}

impl fmt::Display for RenameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFilename => write!(formatter, "path has no filename"),
            Self::NonUnicodeFilename => write!(formatter, "filename is not valid Unicode"),
            Self::PercentDecode(error) => error.fmt(formatter),
            Self::UnsafeDecodedFilename(filename) => {
                write!(
                    formatter,
                    "decoded filename is unsafe on Windows: {filename}"
                )
            }
            Self::TargetExists(path) => {
                write!(
                    formatter,
                    "decoded filename already exists: {}",
                    path.display()
                )
            }
            Self::Filesystem(error) => error.fmt(formatter),
            Self::SourceRemoval { source, rollback } => {
                write!(
                    formatter,
                    "decoded name was created but the original name could not be removed: {source}"
                )?;
                if let Some(rollback) = rollback {
                    write!(formatter, "; rollback also failed: {rollback}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for RenameError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::PercentDecode(error) => Some(error),
            Self::Filesystem(error) => Some(error),
            Self::SourceRemoval { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub fn normalize_new_collection_zip(
    path: &Path,
    parody_evidence: Vec<ParodyEvidence>,
) -> Result<RenameOutcome, RenameError> {
    let original_filename = path.file_name().ok_or(RenameError::MissingFilename)?;
    let original_filename = original_filename
        .to_str()
        .ok_or(RenameError::NonUnicodeFilename)?;
    let Some(decoded_filename) =
        decode_percent_encoded_filename(original_filename).map_err(RenameError::PercentDecode)?
    else {
        return Ok(RenameOutcome::NotPercentEncoded);
    };

    if !is_safe_windows_zip_filename(&decoded_filename) {
        return Err(RenameError::UnsafeDecodedFilename(decoded_filename));
    }

    let parsed = parse_filename(&ParseInput {
        filename: original_filename.to_owned(),
        parody_evidence,
    });
    let has_structure = parsed.event.is_some()
        || parsed.leading_bracket_raw.is_some()
        || parsed.classification.raw_marker.is_some();
    if parsed.parse_status != ParseStatus::Complete || !has_structure {
        return Ok(RenameOutcome::NotStructurallyParsed { decoded_filename });
    }

    let target = path.with_file_name(&decoded_filename);
    if target.try_exists().map_err(RenameError::Filesystem)? {
        return Err(RenameError::TargetExists(target));
    }

    match fs::hard_link(path, &target) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(RenameError::TargetExists(target));
        }
        Err(error) => return Err(RenameError::Filesystem(error)),
    }
    if let Err(source) = fs::remove_file(path) {
        let rollback = fs::remove_file(&target).err();
        return Err(RenameError::SourceRemoval { source, rollback });
    }
    Ok(RenameOutcome::Renamed {
        original: path.to_owned(),
        renamed: target,
    })
}

fn is_safe_windows_zip_filename(filename: &str) -> bool {
    if filename.is_empty()
        || !filename.to_ascii_lowercase().ends_with(".zip")
        || filename.ends_with([' ', '.'])
        || filename
            .chars()
            .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
    {
        return false;
    }

    let basename = filename.split('.').next().unwrap_or_default();
    if basename.ends_with([' ', '.']) {
        return false;
    }
    let uppercase = basename.to_ascii_uppercase();
    !matches!(uppercase.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(uppercase.len() == 4
            && (uppercase.starts_with("COM") || uppercase.starts_with("LPT"))
            && uppercase.as_bytes()[3].is_ascii_digit()
            && uppercase.as_bytes()[3] != b'0')
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        RenameError, RenameOutcome, decode_percent_encoded_filename, normalize_new_collection_zip,
    };

    #[test]
    fn percent_decoding_happens_exactly_once() {
        let decoded = decode_percent_encoded_filename("%2541.zip")
            .expect("valid encoding")
            .expect("decoded filename");

        assert_eq!("%41.zip", decoded);
    }

    #[test]
    fn invalid_percent_encoding_is_rejected() {
        let error = decode_percent_encoded_filename("作品%2.zip").expect_err("invalid escape");

        assert!(matches!(
            error,
            super::PercentDecodeError::InvalidEscape { .. }
        ));
    }

    #[test]
    fn structurally_parsed_zip_is_renamed_without_overwrite() {
        let directory = test_directory("rename");
        fs::create_dir(&directory).expect("create test directory");
        let original = directory.join("%28C77%29%20%5Bcircle%5D%20title.zip");
        fs::write(&original, b"zip placeholder").expect("create source file");

        let outcome = normalize_new_collection_zip(&original, Vec::new()).expect("rename file");
        let renamed = directory.join("(C77) [circle] title.zip");

        assert_eq!(
            RenameOutcome::Renamed {
                original: original.clone(),
                renamed: renamed.clone(),
            },
            outcome
        );
        assert!(!original.exists());
        assert!(renamed.exists());

        fs::remove_file(renamed).expect("remove test file");
        fs::remove_dir(directory).expect("remove test directory");
    }

    #[test]
    fn existing_decoded_target_is_never_overwritten() {
        let directory = test_directory("collision");
        fs::create_dir(&directory).expect("create test directory");
        let original = directory.join("%28C77%29%20%5Bcircle%5D%20title.zip");
        let target = directory.join("(C77) [circle] title.zip");
        fs::write(&original, b"source").expect("create source file");
        fs::write(&target, b"existing target").expect("create target file");

        let error = normalize_new_collection_zip(&original, Vec::new())
            .expect_err("target collision must fail");

        assert!(matches!(error, RenameError::TargetExists(path) if path == target));
        assert_eq!(fs::read(&original).expect("read source"), b"source");
        assert_eq!(fs::read(&target).expect("read target"), b"existing target");

        fs::remove_file(original).expect("remove source");
        fs::remove_file(target).expect("remove target");
        fs::remove_dir(directory).expect("remove test directory");
    }

    #[test]
    fn decoded_path_separator_is_rejected_without_renaming() {
        let directory = test_directory("unsafe");
        fs::create_dir(&directory).expect("create test directory");
        let original = directory.join("%28C77%29%20%5Bcircle%5D%20bad%2Ftitle.zip");
        fs::write(&original, b"source").expect("create source file");

        let error = normalize_new_collection_zip(&original, Vec::new())
            .expect_err("unsafe filename must fail");

        assert!(matches!(error, RenameError::UnsafeDecodedFilename(_)));
        assert!(original.exists());

        fs::remove_file(original).expect("remove source");
        fs::remove_dir(directory).expect("remove test directory");
    }

    #[test]
    fn decoded_title_without_structure_keeps_the_original_name() {
        let directory = test_directory("title-only");
        fs::create_dir(&directory).expect("create test directory");
        let original = directory.join("%54itle.zip");
        fs::write(&original, b"source").expect("create source file");

        let outcome =
            normalize_new_collection_zip(&original, Vec::new()).expect("evaluate filename");

        assert_eq!(
            RenameOutcome::NotStructurallyParsed {
                decoded_filename: "Title.zip".to_owned(),
            },
            outcome
        );
        assert!(original.exists());

        fs::remove_file(original).expect("remove source");
        fs::remove_dir(directory).expect("remove test directory");
    }

    fn test_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("doujin-parser-{label}-{unique}"))
    }
}
