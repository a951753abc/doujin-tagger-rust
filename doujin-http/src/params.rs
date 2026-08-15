//! Shared path/query parameter parsing and name mapping helpers.

use std::collections::HashSet;

use doujin_scanner::SourceKind;
use doujin_storage::collections::{
    CollectionQuery, CollectionSort, MissingMetadataField, ReviewQueueKind, ReviewQueueQuery,
    SortDirection,
};
use doujin_storage::external_search_batches::ExternalSearchBatchStrategy;
use doujin_storage::lifecycle::CandidateDecision;
use doujin_storage::metadata::MetadataField;
use doujin_storage::statistics::CollectionFacet;
use doujin_storage::vocabulary::VocabularyField;

use crate::error::ApiError;

pub(crate) fn parse_collection_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_collection_id", "collection ID 必須是正整數"))
}

pub(crate) fn parse_work_basket_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_work_basket_id", "工作籃 ID 必須是正整數"))
}

pub(crate) fn parse_saved_view_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_saved_view_id", "Saved View ID 必須是正整數"))
}

pub(crate) fn parse_tombstone_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_tombstone_id", "tombstone ID 必須是正整數"))
}

pub(crate) fn parse_candidate_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::bad_request("invalid_candidate_id", "candidate ID 必須是正整數"))
}

pub(crate) fn candidate_decision_name(decision: CandidateDecision) -> &'static str {
    match decision {
        CandidateDecision::Pending => "pending",
        CandidateDecision::Confirmed => "confirmed",
        CandidateDecision::Rejected => "rejected",
    }
}

pub(crate) fn parse_metadata_assertion_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_metadata_assertion_id",
                "metadata assertion ID 必須是正整數",
            )
        })
}

pub(crate) fn parse_external_search_job_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_external_search_job_id",
                "external search job ID 必須是正整數",
            )
        })
}

pub(crate) fn parse_external_search_batch_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_external_search_batch_id",
                "external search batch ID 必須是正整數",
            )
        })
}

pub(crate) fn parse_external_search_batch_strategy(
    value: &str,
) -> Result<ExternalSearchBatchStrategy, ApiError> {
    match value {
        "only_missing" => Ok(ExternalSearchBatchStrategy::OnlyMissing),
        "specified" => Ok(ExternalSearchBatchStrategy::Specified),
        _ => Err(ApiError::bad_request(
            "invalid_external_search_batch_strategy",
            "external search batch strategy 必須是 only_missing 或 specified",
        )),
    }
}

pub(crate) fn parse_external_search_fields(
    values: Vec<String>,
) -> Result<Vec<MetadataField>, ApiError> {
    if values.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_external_search_fields",
            "external search 至少必須指定一個 metadata field",
        ));
    }
    let mut fields = Vec::with_capacity(values.len());
    for value in values {
        let field = parse_metadata_field(&value).map_err(|_| {
            ApiError::bad_request(
                "invalid_external_search_fields",
                "external search 包含不支援的 metadata field",
            )
        })?;
        if !fields.contains(&field) {
            fields.push(field);
        }
    }
    Ok(fields)
}

pub(crate) fn parse_metadata_field(value: &str) -> Result<MetadataField, ApiError> {
    match value {
        "title" => Ok(MetadataField::Title),
        "event" => Ok(MetadataField::Event),
        "circle" => Ok(MetadataField::Circle),
        "authors" => Ok(MetadataField::Authors),
        "parody" => Ok(MetadataField::Parody),
        "classification" => Ok(MetadataField::Classification),
        "is_dl" => Ok(MetadataField::IsDl),
        _ => Err(ApiError::bad_request(
            "invalid_metadata_field",
            "不支援指定的 metadata field",
        )),
    }
}

pub(crate) fn positive_u32_or(value: Option<i64>, fallback: u32) -> u32 {
    value
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

pub(crate) fn clamped_per_page(value: Option<i64>) -> u32 {
    value.map(|value| value.clamp(1, 200) as u32).unwrap_or(50)
}

pub(crate) fn parse_facet_query(
    raw_query: Option<&str>,
) -> Result<(CollectionFacet, String, u32), ApiError> {
    let mut field = None;
    let mut search = String::new();
    let mut limit = 20;
    let mut scalar_keys = HashSet::new();
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        let key = key.as_ref();
        let value = value.as_ref();
        match key {
            "field" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                field = Some(match value {
                    "event" => CollectionFacet::Event,
                    "circle" => CollectionFacet::Circle,
                    "author" => CollectionFacet::Author,
                    "parody" => CollectionFacet::Parody,
                    "tag" => CollectionFacet::Tag,
                    _ => {
                        return Err(ApiError::bad_request(
                            "invalid_facet_field",
                            "field 必須是 event、circle、author、parody 或 tag",
                        ));
                    }
                });
            }
            "q" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                search = value.trim().to_owned();
            }
            "limit" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                let parsed = value.parse::<i64>().map_err(|_| {
                    ApiError::bad_request("invalid_facet_limit", "limit 必須是整數")
                })?;
                limit = parsed.clamp(1, 50) as u32;
            }
            _ => {}
        }
    }
    let field = field
        .ok_or_else(|| ApiError::bad_request("missing_facet_field", "facet 查詢必須指定 field"))?;
    Ok((field, search, limit))
}

pub(crate) fn parse_vocabulary_query(
    raw_query: Option<&str>,
) -> Result<Option<VocabularyField>, ApiError> {
    let mut field = None;
    let mut scalar_keys = HashSet::new();
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        if key == "field" {
            ensure_single_parameter(&mut scalar_keys, "field")?;
            field = Some(parse_vocabulary_field(&value)?);
        }
    }
    Ok(field)
}

pub(crate) fn parse_vocabulary_field(value: &str) -> Result<VocabularyField, ApiError> {
    VocabularyField::parse(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_vocabulary_field",
            "field 必須是 event、circle、author 或 parody",
        )
    })
}

pub(crate) fn parse_collection_query(raw_query: Option<&str>) -> Result<CollectionQuery, ApiError> {
    let mut query = CollectionQuery::default();
    let mut scalar_keys = HashSet::new();
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        let key = key.as_ref();
        let value = value.as_ref();
        match key {
            "q" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.search = Some(value.to_owned());
            }
            "page" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                let value = value.parse::<i64>().map_err(|_| invalid_query())?;
                query.page = positive_u32_or(Some(value), 1);
            }
            "per_page" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                let value = value.parse::<i64>().map_err(|_| invalid_query())?;
                query.per_page = clamped_per_page(Some(value));
            }
            "sort" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.sort = match value {
                    "created" => CollectionSort::Created,
                    "updated" => CollectionSort::Updated,
                    "title" => CollectionSort::Title,
                    _ => CollectionSort::default(),
                };
            }
            "direction" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.direction = match value {
                    "asc" => SortDirection::Ascending,
                    "desc" => SortDirection::Descending,
                    _ => SortDirection::default(),
                };
            }
            "event" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.event = Some(required_filter_value(value)?);
            }
            "circle" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.circle = Some(required_filter_value(value)?);
            }
            "author" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.author = Some(required_filter_value(value)?);
            }
            "parody" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.parody = Some(required_filter_value(value)?);
            }
            "classification" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.classification = Some(required_filter_value(value)?);
            }
            "subcategory" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.subcategory = Some(required_filter_value(value)?);
            }
            "source" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.source = Some(match value {
                    "archive" => SourceKind::Archive,
                    "downloads" => SourceKind::Downloads,
                    _ => {
                        return Err(ApiError::bad_request(
                            "invalid_collection_filter",
                            "source 必須是 archive 或 downloads",
                        ));
                    }
                });
            }
            "tag" => query.filters.tags.push(required_filter_value(value)?),
            "untagged" => {
                ensure_single_parameter(&mut scalar_keys, key)?;
                query.filters.untagged = match value {
                    "1" | "true" => true,
                    "" | "0" | "false" => false,
                    _ => {
                        return Err(ApiError::bad_request(
                            "invalid_collection_filter",
                            "untagged 必須是 1、0、true 或 false",
                        ));
                    }
                };
            }
            "missing" => query.filters.missing.push(match value {
                "any" => MissingMetadataField::Any,
                "title" => MissingMetadataField::Title,
                "event" => MissingMetadataField::Event,
                "circle" => MissingMetadataField::Circle,
                "authors" => MissingMetadataField::Authors,
                "parody" => MissingMetadataField::Parody,
                "classification" => MissingMetadataField::Classification,
                _ => {
                    return Err(ApiError::bad_request(
                        "invalid_collection_filter",
                        "missing 必須是 any、title、event、circle、authors、parody 或 classification",
                    ));
                }
            }),
            _ => {}
        }
    }
    Ok(query)
}

pub(crate) fn parse_review_queue_query(
    raw_query: Option<&str>,
) -> Result<ReviewQueueQuery, ApiError> {
    let mut query = ReviewQueueQuery::default();
    let mut scalar_keys = HashSet::new();
    for (key, value) in form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        let key = key.as_ref();
        let value = value.as_ref();
        match key {
            "page" => {
                ensure_single_review_parameter(&mut scalar_keys, key)?;
                let value = value.parse::<i64>().map_err(|_| invalid_review_query())?;
                query.page = positive_u32_or(Some(value), 1);
            }
            "per_page" => {
                ensure_single_review_parameter(&mut scalar_keys, key)?;
                let value = value.parse::<i64>().map_err(|_| invalid_review_query())?;
                query.per_page = value.clamp(1, 100) as u32;
            }
            "kind" => {
                ensure_single_review_parameter(&mut scalar_keys, key)?;
                query.kind = match value {
                    "all" => ReviewQueueKind::All,
                    "missing" => ReviewQueueKind::Missing,
                    "candidate" => ReviewQueueKind::Candidate,
                    _ => return Err(invalid_review_query()),
                };
            }
            _ => {
                return Err(invalid_review_query());
            }
        }
    }
    Ok(query)
}

pub(crate) fn ensure_single_review_parameter(
    scalar_keys: &mut HashSet<String>,
    key: &str,
) -> Result<(), ApiError> {
    if scalar_keys.insert(key.to_owned()) {
        Ok(())
    } else {
        Err(invalid_review_query())
    }
}

pub(crate) fn ensure_single_parameter(
    scalar_keys: &mut HashSet<String>,
    key: &str,
) -> Result<(), ApiError> {
    if scalar_keys.insert(key.to_owned()) {
        Ok(())
    } else {
        Err(invalid_query())
    }
}

pub(crate) fn required_filter_value(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        Err(ApiError::bad_request(
            "invalid_collection_filter",
            "collection filter value 不得為空白",
        ))
    } else {
        Ok(value.to_owned())
    }
}

pub(crate) fn invalid_query() -> ApiError {
    ApiError::bad_request("invalid_query", "collection query 參數無效")
}

pub(crate) fn invalid_review_query() -> ApiError {
    ApiError::bad_request(
        "invalid_review_query",
        "review queue 只支援 page、per_page 與 kind=all|missing|candidate",
    )
}

pub(crate) fn source_name(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Archive => "archive",
        SourceKind::Downloads => "downloads",
    }
}

pub(crate) fn collection_sort_name(sort: CollectionSort) -> &'static str {
    match sort {
        CollectionSort::Created => "created",
        CollectionSort::Updated => "updated",
        CollectionSort::Title => "title",
    }
}

pub(crate) fn sort_direction_name(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Ascending => "asc",
        SortDirection::Descending => "desc",
    }
}

pub(crate) fn missing_name(field: MissingMetadataField) -> &'static str {
    match field {
        MissingMetadataField::Any => "any",
        MissingMetadataField::Title => "title",
        MissingMetadataField::Event => "event",
        MissingMetadataField::Circle => "circle",
        MissingMetadataField::Authors => "authors",
        MissingMetadataField::Parody => "parody",
        MissingMetadataField::Classification => "classification",
    }
}

pub(crate) fn validated_collection_ids(collection_ids: Vec<i64>) -> Result<Vec<i64>, ApiError> {
    if collection_ids.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_collection_ids",
            "collection_ids 不得為空",
        ));
    }
    let mut unique = HashSet::with_capacity(collection_ids.len());
    if collection_ids
        .iter()
        .any(|collection_id| *collection_id <= 0 || !unique.insert(*collection_id))
    {
        return Err(ApiError::bad_request(
            "invalid_collection_ids",
            "collection_ids 必須是互不重複的正整數",
        ));
    }
    Ok(collection_ids)
}

pub(crate) fn positive_id(value: i64, code: &'static str, message: &str) -> Result<i64, ApiError> {
    if value > 0 {
        Ok(value)
    } else {
        Err(ApiError::bad_request(code, message))
    }
}
