use std::fmt;
use std::fs;
use std::path::Path;

use doujin_files::{BatchReport, ItemResult, ItemStatus, RecycleBin, RenameRequest};
use doujin_storage::path_key;
use serde::{Deserialize, Serialize};

use crate::{ApplicationResult, ApplicationService};

pub const ALLOWED_TOKENS: [&str; 8] = [
    "title",
    "event",
    "circle",
    "authors",
    "parody",
    "classification",
    "subcategory",
    "is_dl",
];

const MAX_WINDOWS_COMPONENT_UTF16: usize = 255;
const MAX_WINDOWS_PATH_UTF16: usize = 259;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenameValues<'a> {
    pub title: Option<&'a str>,
    pub event: Option<&'a str>,
    pub circle: Option<&'a str>,
    pub authors: &'a [String],
    pub parody: Option<&'a str>,
    pub classification: Option<&'a str>,
    pub subcategory: Option<&'a str>,
    pub is_dl: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameTemplateError {
    EmptyTemplate,
    EmptyResult,
    InvalidSyntax,
    UnknownToken(String),
    IllegalCharacter(char),
    ReservedName(String),
    TrailingDotOrSpace,
    ComponentTooLong,
    PathTooLong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenamePreflightStatus {
    Safe,
    Unchanged,
    MissingMetadata,
    Collision,
    Illegal,
    PathTooLong,
    SourceChanged,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenamePreflightItem {
    pub collection_id: i64,
    pub before: String,
    pub after: Option<String>,
    pub expected_source: String,
    pub expected_destination: Option<String>,
    pub status: RenamePreflightStatus,
    pub missing_tokens: Vec<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RenamePreflightSummary {
    pub total: usize,
    pub safe: usize,
    pub unchanged: usize,
    pub missing_metadata: usize,
    pub collision: usize,
    pub illegal: usize,
    pub path_too_long: usize,
    pub source_changed: usize,
    pub unsupported: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenamePreflight {
    pub template: String,
    pub summary: RenamePreflightSummary,
    pub items: Vec<RenamePreflightItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameExpectedItem {
    pub collection_id: i64,
    pub expected_source: String,
    pub expected_destination: String,
}

impl fmt::Display for RenameTemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTemplate => write!(formatter, "rename template 不得為空"),
            Self::EmptyResult => write!(formatter, "缺少 metadata 後 rename 結果為空"),
            Self::InvalidSyntax => write!(formatter, "rename template 大括號語法無效"),
            Self::UnknownToken(token) => write!(formatter, "不支援的 rename token：{token}"),
            Self::IllegalCharacter(character) => {
                write!(formatter, "檔名包含 Windows 非法字元：{character}")
            }
            Self::ReservedName(name) => write!(formatter, "Windows 保留檔名：{name}"),
            Self::TrailingDotOrSpace => write!(formatter, "Windows 檔名不可用句點或空白結尾"),
            Self::ComponentTooLong => write!(formatter, "檔名超過 Windows component 長度限制"),
            Self::PathTooLong => write!(formatter, "完整路徑超過 Windows 相容長度限制"),
        }
    }
}

pub fn render_template(
    template: &str,
    values: &RenameValues<'_>,
) -> Result<String, RenameTemplateError> {
    if template.trim().is_empty() {
        return Err(RenameTemplateError::EmptyTemplate);
    }
    let mut rendered = String::new();
    let mut cursor = 0;
    while let Some(open) = template[cursor..].find('{') {
        let open = cursor + open;
        if template[cursor..open].contains('}') {
            return Err(RenameTemplateError::InvalidSyntax);
        }
        rendered.push_str(&template[cursor..open]);
        let close = template[open + 1..]
            .find('}')
            .map(|offset| open + 1 + offset)
            .ok_or(RenameTemplateError::InvalidSyntax)?;
        let token = &template[open + 1..close];
        if !ALLOWED_TOKENS.contains(&token) {
            return Err(RenameTemplateError::UnknownToken(token.to_owned()));
        }
        if let Some(value) = token_value(token, values) {
            rendered.push_str(&value);
        }
        cursor = close + 1;
    }
    if template[cursor..].contains(['{', '}']) {
        return Err(RenameTemplateError::InvalidSyntax);
    }
    rendered.push_str(&template[cursor..]);
    let rendered = collapse_missing_separators(&rendered);
    if rendered.is_empty() {
        return Err(RenameTemplateError::EmptyResult);
    }
    Ok(rendered)
}

pub fn missing_tokens(
    template: &str,
    values: &RenameValues<'_>,
) -> Result<Vec<String>, RenameTemplateError> {
    // Rendering validates the complete grammar and allowlist first.
    render_template(template, values).or_else(|error| match error {
        RenameTemplateError::EmptyResult => Ok(String::new()),
        other => Err(other),
    })?;
    let mut missing = Vec::new();
    let mut cursor = 0;
    while let Some(open) = template[cursor..].find('{') {
        let open = cursor + open;
        let close = template[open + 1..]
            .find('}')
            .map(|offset| open + 1 + offset)
            .ok_or(RenameTemplateError::InvalidSyntax)?;
        let token = &template[open + 1..close];
        if token_value(token, values).is_none() && !missing.iter().any(|value| value == token) {
            missing.push(token.to_owned());
        }
        cursor = close + 1;
    }
    Ok(missing)
}

pub fn validate_windows_destination(
    parent: &Path,
    filename: &str,
) -> Result<(), RenameTemplateError> {
    let stem = filename
        .strip_suffix(".zip")
        .or_else(|| filename.strip_suffix(".ZIP"))
        .unwrap_or(filename);
    if let Some(character) = filename
        .chars()
        .find(|character| character.is_control() || r#"<>:"/\|?*"#.contains(*character))
    {
        return Err(RenameTemplateError::IllegalCharacter(character));
    }
    if filename.ends_with([' ', '.']) || stem.ends_with([' ', '.']) {
        return Err(RenameTemplateError::TrailingDotOrSpace);
    }
    let reserved = stem.split('.').next().unwrap_or(stem);
    if is_windows_reserved_name(reserved) {
        return Err(RenameTemplateError::ReservedName(reserved.to_owned()));
    }
    if filename.encode_utf16().count() > MAX_WINDOWS_COMPONENT_UTF16 {
        return Err(RenameTemplateError::ComponentTooLong);
    }
    if parent.join(filename).as_os_str().encode_wide_count() > MAX_WINDOWS_PATH_UTF16 {
        return Err(RenameTemplateError::PathTooLong);
    }
    Ok(())
}

fn token_value(token: &str, values: &RenameValues<'_>) -> Option<String> {
    let value = match token {
        "title" => values.title.map(str::to_owned),
        "event" => values.event.map(str::to_owned),
        "circle" => values.circle.map(str::to_owned),
        "authors" => (!values.authors.is_empty()).then(|| values.authors.join("、")),
        "parody" => values.parody.map(str::to_owned),
        "classification" => values.classification.map(str::to_owned),
        "subcategory" => values.subcategory.map(str::to_owned),
        "is_dl" => values
            .is_dl
            .and_then(|is_dl| is_dl.then(|| "DL版".to_owned())),
        _ => None,
    }?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn collapse_missing_separators(value: &str) -> String {
    let mut value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    for empty_group in ["[]", "()", "【】", "（）"] {
        value = value.replace(empty_group, "");
    }
    for doubled in ["--", "——", "··", "・・"] {
        while value.contains(doubled) {
            value = value.replace(doubled, &doubled[..doubled.len() / 2]);
        }
    }
    for doubled in ["- -", "— —", "· ·", "・ ・"] {
        while value.contains(doubled) {
            value = value.replace(doubled, &doubled[..doubled.len() / 2]);
        }
    }
    value
        .trim_matches(|character: char| character.is_whitespace() || "-_·・".contains(character))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_windows_reserved_name(value: &str) -> bool {
    matches!(
        value.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

trait WindowsPathLength {
    fn encode_wide_count(&self) -> usize;
}

impl WindowsPathLength for std::ffi::OsStr {
    fn encode_wide_count(&self) -> usize {
        self.to_string_lossy().encode_utf16().count()
    }
}

impl<R: RecycleBin> ApplicationService<R> {
    pub fn rename_preflight(
        &self,
        collection_ids: &[i64],
        template: &str,
    ) -> ApplicationResult<RenamePreflight> {
        // Validate template before doing collection work, including the case where
        // every token would be missing for a default value set.
        missing_tokens(template, &RenameValues::default())
            .map_err(|error| crate::ApplicationError::InvalidSettings(error.to_string()))?;
        let mut items = Vec::with_capacity(collection_ids.len());
        for collection_id in collection_ids {
            items.push(self.plan_collection_rename(*collection_id, template)?);
        }
        mark_batch_collisions(&mut items);
        Ok(RenamePreflight {
            template: template.to_owned(),
            summary: summarize(&items),
            items,
        })
    }

    pub fn apply_rename_preflight(
        &mut self,
        template: &str,
        expected_items: &[RenameExpectedItem],
    ) -> ApplicationResult<BatchReport> {
        let collection_ids = expected_items
            .iter()
            .map(|item| item.collection_id)
            .collect::<Vec<_>>();
        let fresh = self.rename_preflight(&collection_ids, template)?;
        let mut requests = Vec::new();
        let mut request_positions = Vec::new();
        let mut results = Vec::with_capacity(expected_items.len());
        for (index, (expected, current)) in expected_items.iter().zip(fresh.items).enumerate() {
            let current_destination = current.expected_destination.as_deref();
            if current.status != RenamePreflightStatus::Safe
                || current.expected_source != expected.expected_source
                || current_destination != Some(expected.expected_destination.as_str())
            {
                let message = if current.expected_source != expected.expected_source
                    || current_destination != Some(expected.expected_destination.as_str())
                {
                    "rename preflight 已過期；來源或預計名稱已變更，請重新預覽".to_owned()
                } else {
                    current
                        .message
                        .unwrap_or_else(|| "此項目目前不可安全改名".to_owned())
                };
                results.push(Some(ItemResult {
                    collection_id: expected.collection_id,
                    operation_id: None,
                    status: ItemStatus::Failed,
                    error: Some(message),
                }));
                continue;
            }
            results.push(None);
            request_positions.push(index);
            requests.push(RenameRequest {
                collection_id: expected.collection_id,
                expected_source: expected.expected_source.clone().into(),
                destination: expected.expected_destination.clone().into(),
            });
        }
        let applied = self.rename_collections(&requests);
        for (position, result) in request_positions.into_iter().zip(applied.items) {
            results[position] = Some(result);
        }
        Ok(BatchReport {
            items: results
                .into_iter()
                .map(|result| result.expect("every rename result is populated"))
                .collect(),
        })
    }

    fn plan_collection_rename(
        &self,
        collection_id: i64,
        template: &str,
    ) -> ApplicationResult<RenamePreflightItem> {
        let collection = self.repository.collection(collection_id)?;
        let media_kind = self.repository.collection_media_kind(collection_id)?;
        let source = collection.path.clone();
        let before = collection.filename.clone();
        let base = RenamePreflightItem {
            collection_id,
            before,
            after: None,
            expected_source: source.to_string_lossy().into_owned(),
            expected_destination: None,
            status: RenamePreflightStatus::Safe,
            missing_tokens: Vec::new(),
            message: None,
        };
        if media_kind != "zip" {
            return Ok(blocked_item(
                base,
                RenamePreflightStatus::Unsupported,
                "圖片資料夾尚未接入正式檔案操作 lifecycle；第一版只支援 ZIP",
            ));
        }
        let source_metadata = fs::symlink_metadata(&source);
        if !source_metadata
            .as_ref()
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        {
            return Ok(blocked_item(
                base,
                RenamePreflightStatus::SourceChanged,
                "catalog current path 已不存在或不是一般 ZIP 檔案",
            ));
        }
        let values = RenameValues {
            title: collection.title.as_deref(),
            event: collection.event.as_deref(),
            circle: collection.circle.as_deref(),
            authors: &collection.authors,
            parody: collection.parody.as_deref(),
            classification: collection.classification_top.as_deref(),
            subcategory: collection.classification_subcategory.as_deref(),
            is_dl: collection.is_dl,
        };
        let missing = missing_tokens(template, &values)
            .map_err(|error| crate::ApplicationError::InvalidSettings(error.to_string()))?;
        let rendered = match render_template(template, &values) {
            Ok(rendered) => rendered,
            Err(RenameTemplateError::EmptyResult) => {
                let mut item = blocked_item(
                    base,
                    RenamePreflightStatus::MissingMetadata,
                    "template 使用的 metadata 皆缺少，結果為空",
                );
                item.missing_tokens = missing;
                return Ok(item);
            }
            Err(error) => {
                return Err(crate::ApplicationError::InvalidSettings(error.to_string()));
            }
        };
        let filename = if rendered.to_ascii_lowercase().ends_with(".zip") {
            rendered
        } else {
            format!("{rendered}.zip")
        };
        let parent = source.parent().unwrap_or(Path::new(""));
        let destination = parent.join(&filename);
        let mut item = RenamePreflightItem {
            after: Some(filename),
            expected_destination: Some(destination.to_string_lossy().into_owned()),
            missing_tokens: missing,
            ..base
        };
        if path_key(&source) == path_key(&destination) {
            item.status = RenamePreflightStatus::Unchanged;
            return Ok(item);
        }
        if let Err(error) =
            validate_windows_destination(parent, item.after.as_deref().unwrap_or(""))
        {
            item.status = match error {
                RenameTemplateError::ComponentTooLong | RenameTemplateError::PathTooLong => {
                    RenamePreflightStatus::PathTooLong
                }
                _ => RenamePreflightStatus::Illegal,
            };
            item.message = Some(error.to_string());
            return Ok(item);
        }
        if destination.exists()
            || self
                .repository
                .collection_id_for_current_path(&destination)?
                .is_some()
        {
            item.status = RenamePreflightStatus::Collision;
            item.message = Some("目標名稱已存在；不會覆寫檔案或其他收藏".to_owned());
        }
        Ok(item)
    }
}

fn blocked_item(
    mut item: RenamePreflightItem,
    status: RenamePreflightStatus,
    message: &str,
) -> RenamePreflightItem {
    item.status = status;
    item.message = Some(message.to_owned());
    item
}

fn mark_batch_collisions(items: &mut [RenamePreflightItem]) {
    use std::collections::HashMap;
    let mut destinations: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        if item.status == RenamePreflightStatus::Safe
            && let Some(destination) = &item.expected_destination
        {
            destinations
                .entry(path_key(Path::new(destination)))
                .or_default()
                .push(index);
        }
    }
    for indexes in destinations
        .into_values()
        .filter(|indexes| indexes.len() > 1)
    {
        for index in indexes {
            items[index].status = RenamePreflightStatus::Collision;
            items[index].message = Some("批次內有多筆收藏會產生相同目標名稱".to_owned());
        }
    }
}

fn summarize(items: &[RenamePreflightItem]) -> RenamePreflightSummary {
    let mut summary = RenamePreflightSummary {
        total: items.len(),
        ..RenamePreflightSummary::default()
    };
    for item in items {
        match item.status {
            RenamePreflightStatus::Safe => summary.safe += 1,
            RenamePreflightStatus::Unchanged => summary.unchanged += 1,
            RenamePreflightStatus::MissingMetadata => summary.missing_metadata += 1,
            RenamePreflightStatus::Collision => summary.collision += 1,
            RenamePreflightStatus::Illegal => summary.illegal += 1,
            RenamePreflightStatus::PathTooLong => summary.path_too_long += 1,
            RenamePreflightStatus::SourceChanged => summary.source_changed += 1,
            RenamePreflightStatus::Unsupported => summary.unsupported += 1,
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn allowlist_renders_all_tokens() {
        let authors = vec!["甲".to_owned(), "乙".to_owned()];
        let values = RenameValues {
            title: Some("作品"),
            event: Some("C106"),
            circle: Some("社團"),
            authors: &authors,
            parody: Some("原作"),
            classification: Some("同人誌"),
            subcategory: Some("男性向"),
            is_dl: Some(true),
        };
        assert_eq!(
            "C106 [社團] 作品 甲、乙 原作 同人誌 男性向 DL版",
            render_template(
                "{event} [{circle}] {title} {authors} {parody} {classification} {subcategory} {is_dl}",
                &values
            )
            .expect("render")
        );
    }

    #[test]
    fn rejects_unknown_tokens_and_invalid_braces() {
        let values = RenameValues::default();
        assert_eq!(
            Err(RenameTemplateError::UnknownToken("script".to_owned())),
            render_template("{script}", &values)
        );
        assert_eq!(
            Err(RenameTemplateError::InvalidSyntax),
            render_template("{title", &values)
        );
    }

    #[test]
    fn missing_tokens_are_skipped_without_empty_groups_or_double_separators() {
        let values = RenameValues {
            title: Some("作品"),
            ..RenameValues::default()
        };
        assert_eq!(
            "作品",
            render_template("{event} [{circle}] -- {title} -- {is_dl}", &values)
                .expect("skip missing")
        );
        assert_eq!(
            "同人誌 - 作品",
            render_template(
                "{classification} - {event} - {title}",
                &RenameValues {
                    classification: Some("同人誌"),
                    title: Some("作品"),
                    ..RenameValues::default()
                }
            )
            .expect("collapse missing middle token")
        );
        assert_eq!(
            Err(RenameTemplateError::EmptyResult),
            render_template("[{circle}]", &RenameValues::default())
        );
    }

    #[test]
    fn validates_windows_names_components_and_paths() {
        let parent = Path::new(r"C:\library");
        assert!(validate_windows_destination(parent, "作品.zip").is_ok());
        assert_eq!(
            Err(RenameTemplateError::IllegalCharacter(':')),
            validate_windows_destination(parent, "作品:一.zip")
        );
        assert_eq!(
            Err(RenameTemplateError::ReservedName("CON".to_owned())),
            validate_windows_destination(parent, "CON.zip")
        );
        assert_eq!(
            Err(RenameTemplateError::TrailingDotOrSpace),
            validate_windows_destination(parent, "作品 .zip")
        );
        assert_eq!(
            Err(RenameTemplateError::ComponentTooLong),
            validate_windows_destination(parent, &format!("{}.zip", "字".repeat(252)))
        );
        let long_parent = PathBuf::from(format!(r"C:\{}", "長".repeat(255)));
        assert_eq!(
            Err(RenameTemplateError::PathTooLong),
            validate_windows_destination(&long_parent, "作品.zip")
        );
    }
}
