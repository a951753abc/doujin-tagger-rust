use doujin_parser::domain::{Authors, Classification, Parody};

use crate::{CatalogRepository, StorageError, StorageResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataField {
    Title,
    Event,
    Circle,
    Authors,
    Parody,
    Classification,
    IsDl,
}

impl MetadataField {
    pub const ALL: [Self; 7] = [
        Self::Title,
        Self::Event,
        Self::Circle,
        Self::Authors,
        Self::Parody,
        Self::Classification,
        Self::IsDl,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Event => "event",
            Self::Circle => "circle",
            Self::Authors => "authors",
            Self::Parody => "parody",
            Self::Classification => "classification",
            Self::IsDl => "is_dl",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "title" => Ok(Self::Title),
            "event" => Ok(Self::Event),
            "circle" => Ok(Self::Circle),
            "authors" => Ok(Self::Authors),
            "parody" => Ok(Self::Parody),
            "classification" => Ok(Self::Classification),
            "is_dl" => Ok(Self::IsDl),
            _ => Err(format!("未知 metadata field：{value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataValue {
    Text(String),
    Authors(Authors),
    Parody(Parody),
    Classification(Classification),
    Boolean(bool),
}

impl MetadataValue {
    pub(crate) fn into_json_for(self, field: MetadataField) -> Result<String, String> {
        match (field, &self) {
            (
                MetadataField::Title | MetadataField::Event | MetadataField::Circle,
                Self::Text(value),
            ) if !value.trim().is_empty() => {}
            (MetadataField::Authors, Self::Authors(authors))
                if !authors.values.is_empty()
                    && authors.values.iter().all(|value| !value.trim().is_empty()) => {}
            (MetadataField::Parody, Self::Parody(parody))
                if !parody.raw.trim().is_empty() && !parody.canonical.trim().is_empty() => {}
            (MetadataField::Classification, Self::Classification(classification))
                if !classification.top_level.trim().is_empty() => {}
            (MetadataField::IsDl, Self::Boolean(_)) => {}
            _ => {
                return Err(format!(
                    "欄位 {} 的 metadata value 型別或內容無效",
                    field.as_str()
                ));
            }
        }
        serde_json::to_string(&self).map_err(|error| error.to_string())
    }
}

impl serde::Serialize for MetadataValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Text(value) => value.serialize(serializer),
            Self::Authors(value) => value.serialize(serializer),
            Self::Parody(value) => value.serialize(serializer),
            Self::Classification(value) => value.serialize(serializer),
            Self::Boolean(value) => value.serialize(serializer),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfidenceEvidence {
    pub total: f64,
    pub source_reliability: f64,
    pub identifier_match: f64,
    pub string_similarity: f64,
    pub rule_certainty: f64,
    pub reliable_identifier_exact_match: bool,
    pub reason: String,
}

impl ConfidenceEvidence {
    pub(crate) fn validate_and_encode(&self) -> Result<String, String> {
        for (name, value) in [
            ("total", self.total),
            ("source_reliability", self.source_reliability),
            ("identifier_match", self.identifier_match),
            ("string_similarity", self.string_similarity),
            ("rule_certainty", self.rule_certainty),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(format!("confidence {name} 必須介於 0 到 1"));
            }
        }
        if self.reason.trim().is_empty() {
            return Err("confidence 必須包含人類可讀的理由".to_owned());
        }
        Ok(serde_json::json!({
            "source_reliability": self.source_reliability,
            "identifier_match": self.identifier_match,
            "string_similarity": self.string_similarity,
            "rule_certainty": self.rule_certainty,
            "reliable_identifier_exact_match": self.reliable_identifier_exact_match,
            "reason": self.reason,
        })
        .to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalCandidate {
    pub collection_id: i64,
    pub field: MetadataField,
    pub value: MetadataValue,
    pub source_reference: String,
    pub confidence: ConfidenceEvidence,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalTag {
    pub collection_id: i64,
    pub name: String,
    pub source_reference: String,
    pub confidence: ConfidenceEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalTagOutcome {
    Applied { tag_id: i64 },
    Existing { tag_id: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalCandidateOutcome {
    SearchOnly {
        search_result_id: i64,
    },
    Suggestion {
        search_result_id: i64,
        assertion_id: i64,
    },
    AutoApplied {
        search_result_id: i64,
        assertion_id: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataSource {
    Manual,
    Legacy,
    External,
    Filename,
    Inference,
}

impl MetadataSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Legacy => "legacy",
            Self::External => "external",
            Self::Filename => "filename",
            Self::Inference => "inference",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "manual" => Ok(Self::Manual),
            "legacy" => Ok(Self::Legacy),
            "external" => Ok(Self::External),
            "filename" => Ok(Self::Filename),
            "inference" => Ok(Self::Inference),
            _ => Err(format!("未知 metadata source：{value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionSnapshot {
    pub assertion_id: i64,
    pub source: MetadataSource,
    pub selected_manually: bool,
    pub value_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataAssertionStatus {
    Candidate,
    Accepted,
    Rejected,
    Obsolete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataAssertionDecision {
    Select,
    Reject,
}

impl MetadataAssertionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Obsolete => "obsolete",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "candidate" => Ok(Self::Candidate),
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            "obsolete" => Ok(Self::Obsolete),
            _ => Err(format!("未知 metadata assertion status：{value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataSelectionKind {
    Priority,
    Manual,
    Migration,
}

impl MetadataSelectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Manual => "manual",
            Self::Migration => "migration",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "priority" => Ok(Self::Priority),
            "manual" => Ok(Self::Manual),
            "migration" => Ok(Self::Migration),
            _ => Err(format!("未知 metadata selection kind：{value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExternalSearchDisposition {
    SearchOnly,
    Suggestion,
    AutoApplied,
}

impl ExternalSearchDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SearchOnly => "search_only",
            Self::Suggestion => "suggestion",
            Self::AutoApplied => "auto_applied",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "search_only" => Ok(Self::SearchOnly),
            "suggestion" => Ok(Self::Suggestion),
            "auto_applied" => Ok(Self::AutoApplied),
            _ => Err(format!("未知 external search disposition：{value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataSelectionHistory {
    pub assertion_id: i64,
    pub selected_by: MetadataSelectionKind,
    pub selected_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetadataAssertionHistory {
    pub id: i64,
    pub value_json: String,
    pub source: MetadataSource,
    pub parser_run_id: Option<i64>,
    pub source_reference: Option<String>,
    pub confidence_total: Option<f64>,
    pub confidence_json: Option<String>,
    pub status: MetadataAssertionStatus,
    pub reason: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSearchResultHistory {
    pub id: i64,
    pub value_json: String,
    pub source_reference: String,
    pub confidence_total: f64,
    pub confidence_json: String,
    pub disposition: ExternalSearchDisposition,
    pub assertion_id: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetadataFieldHistory {
    pub field: MetadataField,
    pub selection: Option<MetadataSelectionHistory>,
    pub assertions: Vec<MetadataAssertionHistory>,
    pub external_search_results: Vec<ExternalSearchResultHistory>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetadataHistory {
    pub collection_id: i64,
    pub fields: Vec<MetadataFieldHistory>,
}

impl CatalogRepository {
    pub fn metadata_history(&self, collection_id: i64) -> StorageResult<MetadataHistory> {
        self.collection_status(collection_id)?;
        let mut fields = MetadataField::ALL
            .into_iter()
            .map(|field| MetadataFieldHistory {
                field,
                selection: None,
                assertions: Vec::new(),
                external_search_results: Vec::new(),
            })
            .collect::<Vec<_>>();

        let selections = {
            let mut statement = self.connection.prepare(
                "SELECT field_name, assertion_id, selected_by, selected_at
                 FROM metadata_selections WHERE collection_id = ?1",
            )?;
            statement
                .query_map([collection_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (field, assertion_id, selected_by, selected_at) in selections {
            history_field_mut(&mut fields, &field)?.selection = Some(MetadataSelectionHistory {
                assertion_id,
                selected_by: MetadataSelectionKind::parse(&selected_by)
                    .map_err(StorageError::InvalidSchema)?,
                selected_at,
            });
        }

        let assertions = {
            let mut statement = self.connection.prepare(
                "SELECT id, field_name, value_json, source_kind, parser_run_id,
                        source_reference, confidence_total, confidence_json, status, reason,
                        created_at
                 FROM metadata_assertions
                 WHERE collection_id = ?1 ORDER BY id DESC",
            )?;
            statement
                .query_map([collection_id], map_assertion_history_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        for assertion in assertions {
            let field = assertion.field.clone();
            history_field_mut(&mut fields, &field)?
                .assertions
                .push(MetadataAssertionHistory {
                    id: assertion.id,
                    value_json: assertion.value_json,
                    source: MetadataSource::parse(&assertion.source)
                        .map_err(StorageError::InvalidSchema)?,
                    parser_run_id: assertion.parser_run_id,
                    source_reference: assertion.source_reference,
                    confidence_total: assertion.confidence_total,
                    confidence_json: assertion.confidence_json,
                    status: MetadataAssertionStatus::parse(&assertion.status)
                        .map_err(StorageError::InvalidSchema)?,
                    reason: assertion.reason,
                    created_at: assertion.created_at,
                });
        }

        let search_results = {
            let mut statement = self.connection.prepare(
                "SELECT id, field_name, value_json, source_reference, confidence_total,
                        confidence_json, disposition, assertion_id, created_at
                 FROM external_search_results
                 WHERE collection_id = ?1 ORDER BY id DESC",
            )?;
            statement
                .query_map([collection_id], map_search_result_history_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        for result in search_results {
            let field = result.field.clone();
            history_field_mut(&mut fields, &field)?
                .external_search_results
                .push(ExternalSearchResultHistory {
                    id: result.id,
                    value_json: result.value_json,
                    source_reference: result.source_reference,
                    confidence_total: result.confidence_total,
                    confidence_json: result.confidence_json,
                    disposition: ExternalSearchDisposition::parse(&result.disposition)
                        .map_err(StorageError::InvalidSchema)?,
                    assertion_id: result.assertion_id,
                    created_at: result.created_at,
                });
        }
        for field in &mut fields {
            collapse_duplicate_external_assertions(field);
        }

        Ok(MetadataHistory {
            collection_id,
            fields,
        })
    }
}

fn collapse_duplicate_external_assertions(field: &mut MetadataFieldHistory) {
    let selected_id = field
        .selection
        .as_ref()
        .map(|selection| selection.assertion_id);
    let mut unique = Vec::<MetadataAssertionHistory>::new();
    for assertion in std::mem::take(&mut field.assertions) {
        if assertion.source != MetadataSource::External {
            unique.push(assertion);
            continue;
        }
        let duplicate_index = unique.iter().position(|existing| {
            existing.source == MetadataSource::External
                && existing.value_json == assertion.value_json
                && existing.source_reference == assertion.source_reference
                && existing.confidence_total == assertion.confidence_total
                && existing.confidence_json == assertion.confidence_json
        });
        let Some(index) = duplicate_index else {
            unique.push(assertion);
            continue;
        };
        if assertion_display_rank(&assertion, selected_id)
            > assertion_display_rank(&unique[index], selected_id)
        {
            unique[index] = assertion;
        }
    }
    unique.sort_by_key(|assertion| std::cmp::Reverse(assertion.id));
    field.assertions = unique;
}

fn assertion_display_rank(
    assertion: &MetadataAssertionHistory,
    selected_id: Option<i64>,
) -> (bool, u8, i64) {
    let status = match assertion.status {
        MetadataAssertionStatus::Accepted => 4,
        MetadataAssertionStatus::Candidate => 3,
        MetadataAssertionStatus::Rejected => 2,
        MetadataAssertionStatus::Obsolete => 1,
    };
    (selected_id == Some(assertion.id), status, assertion.id)
}

struct RawAssertionHistory {
    id: i64,
    field: String,
    value_json: String,
    source: String,
    parser_run_id: Option<i64>,
    source_reference: Option<String>,
    confidence_total: Option<f64>,
    confidence_json: Option<String>,
    status: String,
    reason: Option<String>,
    created_at: String,
}

fn map_assertion_history_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawAssertionHistory> {
    Ok(RawAssertionHistory {
        id: row.get(0)?,
        field: row.get(1)?,
        value_json: row.get(2)?,
        source: row.get(3)?,
        parser_run_id: row.get(4)?,
        source_reference: row.get(5)?,
        confidence_total: row.get(6)?,
        confidence_json: row.get(7)?,
        status: row.get(8)?,
        reason: row.get(9)?,
        created_at: row.get(10)?,
    })
}

struct RawSearchResultHistory {
    id: i64,
    field: String,
    value_json: String,
    source_reference: String,
    confidence_total: f64,
    confidence_json: String,
    disposition: String,
    assertion_id: Option<i64>,
    created_at: String,
}

fn map_search_result_history_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<RawSearchResultHistory> {
    Ok(RawSearchResultHistory {
        id: row.get(0)?,
        field: row.get(1)?,
        value_json: row.get(2)?,
        source_reference: row.get(3)?,
        confidence_total: row.get(4)?,
        confidence_json: row.get(5)?,
        disposition: row.get(6)?,
        assertion_id: row.get(7)?,
        created_at: row.get(8)?,
    })
}

fn history_field_mut<'a>(
    fields: &'a mut [MetadataFieldHistory],
    field_name: &str,
) -> StorageResult<&'a mut MetadataFieldHistory> {
    let field = MetadataField::parse(field_name).map_err(StorageError::InvalidSchema)?;
    fields
        .iter_mut()
        .find(|history| history.field == field)
        .ok_or_else(|| {
            StorageError::InvalidSchema(format!("缺少 metadata history 欄位：{field_name}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_duplicate_external_assertion_is_the_single_displayed_candidate() {
        let duplicate = |id, status| {
            MetadataAssertionHistory {
            id,
            value_json: r#"{"raw":"オリジナル作品","canonical":"オリジナル","evidence":"dlsite_exact_title:work_options:ORW"}"#.to_owned(),
            source: MetadataSource::External,
            parser_run_id: None,
            source_reference: Some(
                "https://www.dlsite.com/maniax/work/=/product_id/RJ338758.html".to_owned(),
            ),
            confidence_total: Some(0.85),
            confidence_json: Some(r#"{"identifier_match":0.0}"#.to_owned()),
            status,
            reason: Some("same evidence".to_owned()),
            created_at: format!("2026-08-13T00:0{id}:00Z"),
        }
        };
        let mut field = MetadataFieldHistory {
            field: MetadataField::Parody,
            selection: Some(MetadataSelectionHistory {
                assertion_id: 2,
                selected_by: MetadataSelectionKind::Manual,
                selected_at: "2026-08-13T00:03:00Z".to_owned(),
            }),
            assertions: vec![
                duplicate(2, MetadataAssertionStatus::Accepted),
                duplicate(1, MetadataAssertionStatus::Candidate),
            ],
            external_search_results: Vec::new(),
        };

        collapse_duplicate_external_assertions(&mut field);

        assert_eq!(1, field.assertions.len());
        assert_eq!(2, field.assertions[0].id);
        assert_eq!(
            MetadataAssertionStatus::Accepted,
            field.assertions[0].status
        );
    }
}
