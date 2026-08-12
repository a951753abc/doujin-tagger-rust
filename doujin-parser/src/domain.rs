use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParodyEvidence {
    pub raw: String,
    pub kind: String,
    pub canonical: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseInput {
    pub filename: String,
    pub parody_evidence: Vec<ParodyEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    pub top_level: String,
    pub subcategory: Option<String>,
    pub raw_marker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authors {
    pub raw: Option<String>,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parody {
    pub raw: String,
    pub canonical: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identifier {
    pub scheme: String,
    pub value: String,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtherInfo {
    pub raw: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IgnoredSegment {
    pub raw: String,
    pub kind: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseStatus {
    Complete,
    Partial,
    TitleOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    None,
    ExternalMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseResult {
    pub classification: Classification,
    pub event: Option<String>,
    pub leading_bracket_raw: Option<String>,
    pub circle: Option<String>,
    pub authors: Authors,
    pub title: String,
    pub parody: Option<Parody>,
    pub identifiers: Vec<Identifier>,
    pub other_info: Vec<OtherInfo>,
    pub ignored_segments: Vec<IgnoredSegment>,
    pub is_dl: bool,
    pub parse_status: ParseStatus,
    pub next_action: NextAction,
}
