use crate::domain::{
    Authors, Classification, Identifier, IgnoredSegment, NextAction, OtherInfo, Parody,
    ParodyEvidence, ParseInput, ParseResult, ParseStatus,
};
use crate::filename::decode_percent_encoded_filename;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixParseResult {
    pub classification: Classification,
    pub event: Option<String>,
    pub remaining: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatorParseResult {
    pub leading_bracket_raw: Option<String>,
    pub circle: Option<String>,
    pub authors: Authors,
    pub identifiers: Vec<Identifier>,
    pub other_info: Vec<OtherInfo>,
    pub ignored_segments: Vec<IgnoredSegment>,
    pub parse_status: ParseStatus,
    pub next_action: NextAction,
    pub remaining: String,
}

struct TailParseResult {
    title: String,
    parody: Option<Parody>,
    other_info: Vec<OtherInfo>,
    ignored_segments: Vec<IgnoredSegment>,
    is_dl: bool,
}

struct StrippedTailMarkers<'a> {
    remaining: &'a str,
    other_info: Vec<OtherInfo>,
    ignored_segments: Vec<IgnoredSegment>,
    is_dl: bool,
}

pub fn parse_filename(input: &ParseInput) -> ParseResult {
    let (decoded_filename, mut normalization_info) =
        match decode_percent_encoded_filename(&input.filename) {
            Ok(decoded) => (decoded, Vec::new()),
            Err(_) => (
                None,
                vec![OtherInfo {
                    raw: input.filename.clone(),
                    reason: "invalid_percent_encoding".to_owned(),
                }],
            ),
        };
    let filename = decoded_filename.as_deref().unwrap_or(&input.filename);
    let (filename, source_marker) = strip_supported_source_marker(filename);
    let prefix = parse_prefix_prepared(filename);
    let creator = parse_creator_prefix(&prefix.remaining, &prefix.classification);
    let tail = parse_tail(&creator.remaining, &input.parody_evidence);

    normalization_info.extend(creator.other_info);
    let mut other_info = normalization_info;
    other_info.extend(tail.other_info);
    let mut ignored_segments = Vec::new();
    if let Some(raw) = source_marker {
        ignored_segments.push(IgnoredSegment {
            raw: raw.to_owned(),
            kind: "source_marker".to_owned(),
        });
    }
    ignored_segments.extend(creator.ignored_segments);
    ignored_segments.extend(tail.ignored_segments);

    ParseResult {
        classification: prefix.classification,
        event: prefix.event,
        leading_bracket_raw: creator.leading_bracket_raw,
        circle: creator.circle,
        authors: creator.authors,
        title: tail.title,
        parody: tail.parody,
        identifiers: creator.identifiers,
        other_info,
        ignored_segments,
        is_dl: tail.is_dl,
        parse_status: creator.parse_status,
        next_action: creator.next_action,
    }
}

pub fn parse_prefix(filename: &str) -> PrefixParseResult {
    let decoded = decode_percent_encoded_filename(filename).ok().flatten();
    let filename = decoded.as_deref().unwrap_or(filename);
    let (filename, _) = strip_supported_source_marker(filename);
    parse_prefix_prepared(filename)
}

fn parse_prefix_prepared(filename: &str) -> PrefixParseResult {
    let mut remaining = strip_zip_extension(filename).trim_start();
    let mut classification = default_classification();
    let mut event = None;

    while let Some(prefix) = take_parenthesized_prefix(remaining) {
        if let Some(recognized) = classification_from_marker(prefix.content) {
            let is_commercial = recognized.top_level == "商業誌";
            classification = Classification {
                raw_marker: Some(prefix.raw.to_owned()),
                ..recognized
            };
            remaining = prefix.remaining.trim_start();

            if is_commercial {
                break;
            }
            continue;
        }

        if event.is_none() {
            event = Some(prefix.content.trim().to_owned());
            remaining = prefix.remaining.trim_start();
            continue;
        }

        break;
    }

    PrefixParseResult {
        classification,
        event,
        remaining: remaining.trim().to_owned(),
    }
}

fn strip_supported_source_marker(filename: &str) -> (&str, Option<&str>) {
    let trimmed = filename.trim_start();
    let Some(candidate) = take_square_bracket_prefix(trimmed) else {
        return (filename, None);
    };
    if !candidate.content.contains('@') {
        return (filename, None);
    }

    let after_candidate = candidate.remaining.trim_start();
    let after_prefix = parse_prefix_prepared(after_candidate);
    if take_square_bracket_prefix(after_prefix.remaining.trim_start()).is_none() {
        return (filename, None);
    }

    (after_candidate, Some(candidate.raw))
}

pub fn parse_creator_prefix(input: &str, classification: &Classification) -> CreatorParseResult {
    let mut remaining = input.trim_start();
    let mut identifiers = Vec::new();
    let mut ignored_segments = Vec::new();

    while let Some(prefix) = take_square_bracket_prefix(remaining) {
        if let Some(identifier) = rj_identifier(&prefix) {
            identifiers.push(identifier);
            remaining = prefix.remaining.trim_start();
            continue;
        }
        if is_date_marker(prefix.content) {
            ignored_segments.push(IgnoredSegment {
                raw: prefix.raw.to_owned(),
                kind: "date_marker".to_owned(),
            });
            remaining = prefix.remaining.trim_start();
            continue;
        }
        break;
    }

    let Some(prefix) = take_square_bracket_prefix(remaining) else {
        return CreatorParseResult {
            leading_bracket_raw: None,
            circle: None,
            authors: empty_authors(),
            identifiers,
            other_info: Vec::new(),
            ignored_segments,
            parse_status: ParseStatus::Complete,
            next_action: NextAction::None,
            remaining: remaining.trim().to_owned(),
        };
    };

    let bracket = prefix.content.trim();
    remaining = prefix.remaining.trim_start();

    if classification.top_level == "商業誌" {
        return CreatorParseResult {
            leading_bracket_raw: Some(bracket.to_owned()),
            circle: None,
            authors: authors_from_raw(bracket),
            identifiers,
            other_info: Vec::new(),
            ignored_segments,
            parse_status: ParseStatus::Complete,
            next_action: NextAction::None,
            remaining: remaining.trim().to_owned(),
        };
    }

    match creator_parts(bracket) {
        CreatorParts::CircleOnly(circle) => CreatorParseResult {
            leading_bracket_raw: Some(bracket.to_owned()),
            circle: Some(circle.to_owned()),
            authors: empty_authors(),
            identifiers,
            other_info: Vec::new(),
            ignored_segments,
            parse_status: ParseStatus::Complete,
            next_action: NextAction::None,
            remaining: remaining.trim().to_owned(),
        },
        CreatorParts::CircleAndAuthors { circle, authors } => CreatorParseResult {
            leading_bracket_raw: Some(bracket.to_owned()),
            circle: Some(circle.to_owned()),
            authors: authors_from_raw(authors),
            identifiers,
            other_info: Vec::new(),
            ignored_segments,
            parse_status: ParseStatus::Complete,
            next_action: NextAction::None,
            remaining: remaining.trim().to_owned(),
        },
        CreatorParts::Malformed(reason) => CreatorParseResult {
            leading_bracket_raw: Some(bracket.to_owned()),
            circle: None,
            authors: empty_authors(),
            identifiers,
            other_info: vec![OtherInfo {
                raw: bracket.to_owned(),
                reason: reason.to_owned(),
            }],
            ignored_segments,
            parse_status: ParseStatus::Partial,
            next_action: NextAction::ExternalMetadata,
            remaining: remaining.trim().to_owned(),
        },
    }
}

fn parse_tail(input: &str, parody_evidence: &[ParodyEvidence]) -> TailParseResult {
    let post_markers = strip_trailing_markers(input);
    let mut title = post_markers.remaining;
    let mut parody = None;
    let mut parody_other_info = Vec::new();

    if let Ok(Some(segment)) = trailing_parenthesized_segment(title) {
        let candidate = title[segment.content_start..segment.content_end].trim();
        if let Some(evidence) = parody_evidence
            .iter()
            .find(|evidence| evidence.raw == candidate)
        {
            parody = Some(Parody {
                raw: evidence.raw.clone(),
                canonical: evidence.canonical.clone(),
                evidence: evidence.kind.clone(),
            });
        } else {
            parody_other_info.push(OtherInfo {
                raw: candidate.to_owned(),
                reason: "insufficient_parody_evidence".to_owned(),
            });
        }
        title = &title[..segment.raw_start];
    }

    let mut pre_markers = strip_trailing_markers(title);
    let mut other_info = pre_markers.other_info;
    other_info.extend(parody_other_info);
    other_info.extend(post_markers.other_info);
    pre_markers
        .ignored_segments
        .extend(post_markers.ignored_segments);

    TailParseResult {
        title: normalize_title(pre_markers.remaining),
        parody,
        other_info,
        ignored_segments: pre_markers.ignored_segments,
        is_dl: pre_markers.is_dl || post_markers.is_dl,
    }
}

fn strip_trailing_markers(input: &str) -> StrippedTailMarkers<'_> {
    let mut remaining = input.trim_end();
    let mut reversed_markers = Vec::new();
    let mut reversed_other_info = Vec::new();
    let mut is_dl = false;

    loop {
        if let Some(segment) = trailing_bracket_marker(remaining) {
            let recognized = segment
                .recognized_shape
                .then(|| square_marker_kind(segment.content))
                .flatten();
            if let Some((kind, marks_dl)) = recognized {
                reversed_markers.push(IgnoredSegment {
                    raw: segment.raw.to_owned(),
                    kind: kind.to_owned(),
                });
                is_dl |= marks_dl;
            } else if remaining[..segment.raw_start].trim().is_empty() {
                break;
            } else {
                reversed_other_info.push(OtherInfo {
                    raw: segment.raw.to_owned(),
                    reason: "unclassified_trailing_marker".to_owned(),
                });
            }
            remaining = remaining[..segment.raw_start].trim_end();
            continue;
        }

        if let Ok(Some(segment)) = trailing_parenthesized_segment(remaining) {
            let content = remaining[segment.content_start..segment.content_end].trim();
            if let Some((kind, marks_dl)) = parenthesized_marker_kind(content) {
                reversed_markers.push(IgnoredSegment {
                    raw: remaining[segment.raw_start..].to_owned(),
                    kind: kind.to_owned(),
                });
                is_dl |= marks_dl;
                remaining = remaining[..segment.raw_start].trim_end();
                continue;
            }
        }

        if let Some((before, raw)) = strip_bare_distribution_marker(remaining) {
            reversed_markers.push(IgnoredSegment {
                raw: raw.to_owned(),
                kind: "distribution_marker".to_owned(),
            });
            is_dl = true;
            remaining = before.trim_end();
            continue;
        }

        break;
    }

    reversed_markers.reverse();
    reversed_other_info.reverse();
    StrippedTailMarkers {
        remaining,
        other_info: reversed_other_info,
        ignored_segments: reversed_markers,
        is_dl,
    }
}

fn square_marker_kind(content: &str) -> Option<(&'static str, bool)> {
    let marker = content.trim();
    if marker.eq_ignore_ascii_case("DL版") || marker.eq_ignore_ascii_case("Digital") {
        return Some(("distribution_marker", true));
    }
    if [
        "Chinese",
        "English",
        "Korean",
        "中文",
        "英訳",
        "韓国翻訳",
        "韓国語",
    ]
    .into_iter()
    .any(|known| marker.eq_ignore_ascii_case(known))
    {
        return Some(("language_marker", false));
    }
    if is_date_marker(marker) {
        return Some(("date_marker", false));
    }
    None
}

fn parenthesized_marker_kind(content: &str) -> Option<(&'static str, bool)> {
    let marker = content.trim();
    if ["別スキャン", "修正版", "Full HQ Scan", "画像化済"]
        .into_iter()
        .any(|known| marker.eq_ignore_ascii_case(known))
    {
        return Some(("version_marker", false));
    }
    if ["DL版", "Digital", "デジタル版"]
        .into_iter()
        .any(|known| marker.eq_ignore_ascii_case(known))
    {
        return Some(("distribution_marker", true));
    }
    if ["Chinese", "English", "Korean", "中文"]
        .into_iter()
        .any(|known| marker.eq_ignore_ascii_case(known))
    {
        return Some(("language_marker", false));
    }
    None
}

fn strip_bare_distribution_marker(input: &str) -> Option<(&str, &str)> {
    ["デジタル版", "DL版"].into_iter().find_map(|marker| {
        let before = input.strip_suffix(marker)?;
        let boundary_is_valid = before.chars().next_back().is_none_or(char::is_whitespace);
        boundary_is_valid.then_some((before, marker))
    })
}

fn normalize_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn default_classification() -> Classification {
    Classification {
        top_level: "同人誌".to_owned(),
        subcategory: None,
        raw_marker: None,
    }
}

fn classification_from_marker(content: &str) -> Option<Classification> {
    let marker = content.trim();
    let (top_level, subcategory) = match marker {
        "同人誌" | "同人志" => ("同人誌", None),
        "同人CG" | "同人CG集" => ("CG", None),
        "成年コミック" | "エロ漫画" | "アダルトコミック" => {
            ("商業誌", Some("成年コミック"))
        }
        "官能小説・エロライトノベル" | "官能小説" | "エロライトノベル" => {
            ("商業誌", Some("官能小説"))
        }
        "一般コミック" => ("商業誌", Some("一般コミック")),
        _ => return None,
    };

    Some(Classification {
        top_level: top_level.to_owned(),
        subcategory: subcategory.map(str::to_owned),
        raw_marker: None,
    })
}

fn strip_zip_extension(filename: &str) -> &str {
    let trimmed = filename.trim();
    let Some(suffix) = trimmed.get(trimmed.len().saturating_sub(4)..) else {
        return trimmed;
    };
    if suffix.eq_ignore_ascii_case(".zip") {
        &trimmed[..trimmed.len() - 4]
    } else {
        trimmed
    }
}

fn empty_authors() -> Authors {
    Authors {
        raw: None,
        values: Vec::new(),
    }
}

fn authors_from_raw(raw: &str) -> Authors {
    Authors {
        raw: Some(raw.to_owned()),
        values: raw
            .split(['、', ','])
            .map(str::trim)
            .filter(|author| !author.is_empty())
            .map(str::to_owned)
            .collect(),
    }
}

enum CreatorParts<'a> {
    CircleOnly(&'a str),
    CircleAndAuthors { circle: &'a str, authors: &'a str },
    Malformed(&'static str),
}

fn creator_parts(bracket: &str) -> CreatorParts<'_> {
    if !bracket.chars().any(is_parenthesis) {
        return CreatorParts::CircleOnly(bracket);
    }

    let Ok(trailing) = trailing_parenthesized_segment(bracket) else {
        return CreatorParts::Malformed("malformed_circle_author");
    };
    let Some(segment) = trailing else {
        return CreatorParts::Malformed("author_parenthesis_not_at_tail");
    };

    let circle = bracket[..segment.raw_start].trim();
    let authors = bracket[segment.content_start..segment.content_end].trim();
    if circle.is_empty() || authors.is_empty() {
        return CreatorParts::Malformed("malformed_circle_author");
    }

    CreatorParts::CircleAndAuthors { circle, authors }
}

fn is_parenthesis(character: char) -> bool {
    matches!(character, '(' | ')' | '（' | '）')
}

struct TrailingSegment {
    raw_start: usize,
    content_start: usize,
    content_end: usize,
}

fn trailing_parenthesized_segment(input: &str) -> Result<Option<TrailingSegment>, ()> {
    let trimmed = input.trim_end();
    let mut stack = Vec::new();
    let mut top_level_start = None;
    let mut trailing = None;

    for (index, character) in trimmed.char_indices() {
        match character {
            '(' | '（' => {
                if stack.is_empty() {
                    top_level_start = Some((index, index + character.len_utf8()));
                }
                stack.push(character);
            }
            ')' | '）' => {
                let opening = stack.pop().ok_or(())?;
                if !parentheses_match(opening, character) {
                    return Err(());
                }
                if stack.is_empty() {
                    if index + character.len_utf8() == trimmed.len() {
                        let (raw_start, content_start) = top_level_start.ok_or(())?;
                        trailing = Some(TrailingSegment {
                            raw_start,
                            content_start,
                            content_end: index,
                        });
                    }
                    top_level_start = None;
                }
            }
            _ => {}
        }
    }

    if stack.is_empty() {
        Ok(trailing)
    } else {
        Err(())
    }
}

fn parentheses_match(opening: char, closing: char) -> bool {
    matches!((opening, closing), ('(', ')') | ('（', '）'))
}

fn rj_identifier(prefix: &SquareBracketPrefix<'_>) -> Option<Identifier> {
    let content = prefix.content.trim();
    let (label, digits) = content.split_at_checked(2)?;
    if !label.eq_ignore_ascii_case("RJ")
        || digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    Some(Identifier {
        scheme: "RJ".to_owned(),
        value: format!("RJ{digits}"),
        raw: prefix.raw.to_owned(),
    })
}

fn is_date_marker(content: &str) -> bool {
    let value = content.trim();
    if matches!(value.len(), 6 | 8) && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    if value.len() != 10 {
        return false;
    }

    value.bytes().enumerate().all(|(index, byte)| match index {
        4 | 7 => matches!(byte, b'-' | b'/'),
        _ => byte.is_ascii_digit(),
    })
}

struct ParenthesizedPrefix<'a> {
    raw: &'a str,
    content: &'a str,
    remaining: &'a str,
}

struct SquareBracketPrefix<'a> {
    raw: &'a str,
    content: &'a str,
    remaining: &'a str,
}

struct TrailingBracketMarker<'a> {
    raw_start: usize,
    raw: &'a str,
    content: &'a str,
    recognized_shape: bool,
}

fn trailing_bracket_marker(input: &str) -> Option<TrailingBracketMarker<'_>> {
    let (opening, closing, recognized_shape) = if input.ends_with(']') {
        ('[', ']', true)
    } else if input.ends_with('】') {
        ('【', '】', false)
    } else {
        return None;
    };
    let raw_start = input.rfind(opening)?;
    let content_start = raw_start + opening.len_utf8();
    let content_end = input.len() - closing.len_utf8();
    Some(TrailingBracketMarker {
        raw_start,
        raw: &input[raw_start..],
        content: &input[content_start..content_end],
        recognized_shape,
    })
}

fn take_square_bracket_prefix(input: &str) -> Option<SquareBracketPrefix<'_>> {
    if !input.starts_with('[') {
        return None;
    }
    let closing = input.find(']')?;
    let raw_end = closing + 1;
    Some(SquareBracketPrefix {
        raw: &input[..raw_end],
        content: &input[1..closing],
        remaining: &input[raw_end..],
    })
}

fn take_parenthesized_prefix(input: &str) -> Option<ParenthesizedPrefix<'_>> {
    let first = input.chars().next()?;
    if !matches!(first, '(' | '（') {
        return None;
    }

    let content_start = first.len_utf8();
    let mut depth = 0_u32;
    for (index, character) in input.char_indices() {
        match character {
            '(' | '（' => depth += 1,
            ')' | '）' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let raw_end = index + character.len_utf8();
                    return Some(ParenthesizedPrefix {
                        raw: &input[..raw_end],
                        content: &input[content_start..index],
                        remaining: &input[raw_end..],
                    });
                }
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use crate::domain::{Classification, NextAction, ParodyEvidence, ParseInput, ParseStatus};

    use super::{parse_creator_prefix, parse_filename, parse_prefix};

    #[test]
    fn classification_can_come_before_event() {
        let result = parse_prefix("(同人誌) (C85) [社團] 作品.zip");

        assert_eq!("同人誌", result.classification.top_level);
        assert_eq!(
            Some("(同人誌)"),
            result.classification.raw_marker.as_deref()
        );
        assert_eq!(Some("C85"), result.event.as_deref());
        assert_eq!("[社團] 作品", result.remaining);
    }

    #[test]
    fn classification_can_come_after_event() {
        let result = parse_prefix("(C79) (同人誌) [社團] 作品.zip");

        assert_eq!("同人誌", result.classification.top_level);
        assert_eq!(
            Some("(同人誌)"),
            result.classification.raw_marker.as_deref()
        );
        assert_eq!(Some("C79"), result.event.as_deref());
        assert_eq!("[社團] 作品", result.remaining);
    }

    #[test]
    fn commercial_marker_is_not_an_event() {
        let result = parse_prefix("(成年コミック) [作者] 作品.zip");

        assert_eq!("商業誌", result.classification.top_level);
        assert_eq!(
            Some("成年コミック"),
            result.classification.subcategory.as_deref()
        );
        assert_eq!(None, result.event);
        assert_eq!("[作者] 作品", result.remaining);
    }

    #[test]
    fn fullwidth_parentheses_can_hold_an_event() {
        let result = parse_prefix("（C90） [社團] 作品.zip");

        assert_eq!(Some("C90"), result.event.as_deref());
        assert_eq!("[社團] 作品", result.remaining);
    }

    #[test]
    fn malformed_parentheses_are_left_untouched() {
        let filename = "(C89 [社團 (作者)] 作品.zip";
        let result = parse_prefix(filename);

        assert_eq!(None, result.event);
        assert_eq!("(C89 [社團 (作者)] 作品", result.remaining);
    }

    #[test]
    fn nested_author_parentheses_are_not_split_recursively() {
        let classification = Classification {
            top_level: "同人誌".to_owned(),
            subcategory: None,
            raw_marker: None,
        };
        let result = parse_creator_prefix(
            "[macdoll (士嬢マコ(・c_・ ))] 作品 (オリジナル)",
            &classification,
        );

        assert_eq!(Some("macdoll"), result.circle.as_deref());
        assert_eq!(Some("士嬢マコ(・c_・ )"), result.authors.raw.as_deref());
        assert_eq!(["士嬢マコ(・c_・ )"], result.authors.values.as_slice());
        assert_eq!("作品 (オリジナル)", result.remaining);
    }

    #[test]
    fn broken_creator_parentheses_require_external_metadata() {
        let classification = Classification {
            top_level: "同人誌".to_owned(),
            subcategory: None,
            raw_marker: None,
        };
        let result = parse_creator_prefix(
            "[70 Nenshiki Yuukyuu Kikan (Ohagi-san))] Ripe flower buds",
            &classification,
        );

        assert_eq!(None, result.circle);
        assert_eq!(ParseStatus::Partial, result.parse_status);
        assert_eq!(NextAction::ExternalMetadata, result.next_action);
        assert_eq!("malformed_circle_author", result.other_info[0].reason);
    }

    #[test]
    fn commercial_bracket_is_an_author_list() {
        let classification = Classification {
            top_level: "商業誌".to_owned(),
            subcategory: Some("官能小説".to_owned()),
            raw_marker: Some("(官能小説)".to_owned()),
        };
        let result = parse_creator_prefix("[有機企画、火愚夜] 作品", &classification);

        assert_eq!(None, result.circle);
        assert_eq!(["有機企画", "火愚夜"], result.authors.values.as_slice());
    }

    #[test]
    fn technical_prefixes_are_not_creators() {
        let classification = Classification {
            top_level: "同人誌".to_owned(),
            subcategory: None,
            raw_marker: None,
        };
        let result =
            parse_creator_prefix("[RJ407766] [180529] [社團 (作者)] 作品", &classification);

        assert_eq!("RJ407766", result.identifiers[0].value);
        assert_eq!("[180529]", result.ignored_segments[0].raw);
        assert_eq!(Some("社團"), result.circle.as_deref());
        assert_eq!(["作者"], result.authors.values.as_slice());
    }

    #[test]
    fn unsupported_trailing_parentheses_become_other_information() {
        let result = parse_filename(&ParseInput {
            filename: "[社團] 作品名稱 (角色名稱).zip".to_owned(),
            parody_evidence: Vec::new(),
        });

        assert_eq!("作品名稱", result.title);
        assert!(result.parody.is_none());
        assert_eq!("角色名稱", result.other_info[0].raw);
    }

    #[test]
    fn evidence_promotes_trailing_parentheses_to_parody() {
        let result = parse_filename(&ParseInput {
            filename: "[社團] 作品名稱 (ポケモン).zip".to_owned(),
            parody_evidence: vec![ParodyEvidence {
                raw: "ポケモン".to_owned(),
                kind: "confirmed_alias".to_owned(),
                canonical: "ポケットモンスター".to_owned(),
            }],
        });

        let parody = result.parody.expect("confirmed parody");
        assert_eq!("ポケモン", parody.raw);
        assert_eq!("ポケットモンスター", parody.canonical);
    }

    #[test]
    fn marker_after_parody_does_not_hide_the_parody() {
        let result = parse_filename(&ParseInput {
            filename: "[社團] 作品 (Fate Grand Order) (修正版).zip".to_owned(),
            parody_evidence: vec![ParodyEvidence {
                raw: "Fate Grand Order".to_owned(),
                kind: "confirmed_alias".to_owned(),
                canonical: "Fate/Grand Order".to_owned(),
            }],
        });

        assert_eq!("作品", result.title);
        assert_eq!("Fate/Grand Order", result.parody.unwrap().canonical);
        assert_eq!("(修正版)", result.ignored_segments[0].raw);
    }

    #[test]
    fn bare_dl_marker_before_fullwidth_parody_is_removed() {
        let result = parse_filename(&ParseInput {
            filename: "[社團] 作品 DL版 （原作）.zip".to_owned(),
            parody_evidence: vec![ParodyEvidence {
                raw: "原作".to_owned(),
                kind: "confirmed_dictionary".to_owned(),
                canonical: "原作".to_owned(),
            }],
        });

        assert_eq!("作品", result.title);
        assert!(result.is_dl);
        assert_eq!(None, result.event);
        assert_eq!("DL版", result.ignored_segments[0].raw);
    }

    #[test]
    fn missing_event_without_dl_marker_stays_uncategorized() {
        let result = parse_filename(&ParseInput {
            filename: "[社團] 作品.zip".to_owned(),
            parody_evidence: Vec::new(),
        });

        assert!(!result.is_dl);
        assert_eq!(None, result.event);
    }

    #[test]
    fn bracket_only_title_is_not_consumed_as_an_unknown_marker() {
        let result = parse_filename(&ParseInput {
            filename: "【本編ドラマ】".to_owned(),
            parody_evidence: Vec::new(),
        });

        assert_eq!("【本編ドラマ】", result.title);
        assert!(result.other_info.is_empty());
    }

    #[test]
    fn fullwidth_unknown_tail_is_preserved_outside_the_title() {
        let result = parse_filename(&ParseInput {
            filename: "[社團] 作品【翻譯組】.zip".to_owned(),
            parody_evidence: Vec::new(),
        });

        assert_eq!("作品", result.title);
        assert_eq!("【翻譯組】", result.other_info[0].raw);
        assert_eq!("unclassified_trailing_marker", result.other_info[0].reason);
    }

    #[test]
    fn at_sign_without_later_structure_remains_the_circle() {
        let result = parse_filename(&ParseInput {
            filename: "[artist@example.com] 作品.zip".to_owned(),
            parody_evidence: Vec::new(),
        });

        assert_eq!(Some("artist@example.com"), result.circle.as_deref());
        assert!(result.ignored_segments.is_empty());
    }
}
