//! Metadata history, manual metadata, tag and batch endpoints.

use std::collections::HashSet;
use std::sync::TryLockError;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use doujin_app::{ApplicationBatchOutcome, ApplicationBatchReport};
use doujin_files::RecycleBin;
use doujin_parser::domain::{Authors, Classification, Parody};
use doujin_storage::metadata::{
    ExternalSearchResultHistory, MetadataAssertionDecision, MetadataAssertionHistory,
    MetadataField, MetadataFieldHistory, MetadataHistory, MetadataSelectionHistory, MetadataValue,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::collections::CollectionResponse;
use crate::error::ApiError;
use crate::params::{parse_collection_id, parse_metadata_assertion_id, parse_metadata_field};
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Serialize)]
pub(crate) struct MetadataHistoryResponse {
    collection_id: i64,
    fields: Vec<MetadataFieldHistoryResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MetadataFieldHistoryResponse {
    field: &'static str,
    selection: Option<MetadataSelectionResponse>,
    assertions: Vec<MetadataAssertionResponse>,
    external_search_results: Vec<ExternalSearchResultResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MetadataSelectionResponse {
    assertion_id: i64,
    selected_by: &'static str,
    selected_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct MetadataAssertionResponse {
    id: i64,
    value: Value,
    source: &'static str,
    parser_run_id: Option<i64>,
    source_reference: Option<String>,
    confidence_total: Option<f64>,
    confidence: Option<Value>,
    status: &'static str,
    reason: Option<String>,
    created_at: String,
    selected: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExternalSearchResultResponse {
    id: i64,
    value: Value,
    source_reference: String,
    confidence_total: f64,
    confidence: Value,
    disposition: &'static str,
    assertion_id: Option<i64>,
    created_at: String,
}

impl TryFrom<MetadataHistory> for MetadataHistoryResponse {
    type Error = ApiError;

    fn try_from(history: MetadataHistory) -> Result<Self, Self::Error> {
        Ok(Self {
            collection_id: history.collection_id,
            fields: history
                .fields
                .into_iter()
                .map(metadata_field_history_response)
                .collect::<Result<_, _>>()?,
        })
    }
}

pub(crate) fn metadata_field_history_response(
    history: MetadataFieldHistory,
) -> Result<MetadataFieldHistoryResponse, ApiError> {
    let selected_assertion_id = history
        .selection
        .as_ref()
        .map(|selection| selection.assertion_id);
    Ok(MetadataFieldHistoryResponse {
        field: history.field.as_str(),
        selection: history.selection.map(metadata_selection_response),
        assertions: history
            .assertions
            .into_iter()
            .map(|assertion| metadata_assertion_response(assertion, selected_assertion_id))
            .collect::<Result<_, _>>()?,
        external_search_results: history
            .external_search_results
            .into_iter()
            .map(external_search_result_response)
            .collect::<Result<_, _>>()?,
    })
}

pub(crate) fn metadata_selection_response(
    selection: MetadataSelectionHistory,
) -> MetadataSelectionResponse {
    MetadataSelectionResponse {
        assertion_id: selection.assertion_id,
        selected_by: selection.selected_by.as_str(),
        selected_at: selection.selected_at,
    }
}

pub(crate) fn metadata_assertion_response(
    assertion: MetadataAssertionHistory,
    selected_assertion_id: Option<i64>,
) -> Result<MetadataAssertionResponse, ApiError> {
    let value = serde_json::from_str(&assertion.value_json).map_err(|_| ApiError::internal())?;
    let confidence = assertion
        .confidence_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| ApiError::internal())?;
    Ok(MetadataAssertionResponse {
        id: assertion.id,
        value,
        source: assertion.source.as_str(),
        parser_run_id: assertion.parser_run_id,
        source_reference: assertion.source_reference,
        confidence_total: assertion.confidence_total,
        confidence,
        status: assertion.status.as_str(),
        reason: assertion.reason,
        created_at: assertion.created_at,
        selected: selected_assertion_id == Some(assertion.id),
    })
}

pub(crate) fn external_search_result_response(
    result: ExternalSearchResultHistory,
) -> Result<ExternalSearchResultResponse, ApiError> {
    Ok(ExternalSearchResultResponse {
        id: result.id,
        value: serde_json::from_str(&result.value_json).map_err(|_| ApiError::internal())?,
        source_reference: result.source_reference,
        confidence_total: result.confidence_total,
        confidence: serde_json::from_str(&result.confidence_json)
            .map_err(|_| ApiError::internal())?,
        disposition: result.disposition.as_str(),
        assertion_id: result.assertion_id,
        created_at: result.created_at,
    })
}

pub(crate) async fn get_metadata_history<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
) -> Result<Json<MetadataHistoryResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let history = tokio::task::spawn_blocking(move || {
        let application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .metadata_history(collection_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(history.try_into()?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManualMetadataRequest {
    value: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MetadataAssertionDecisionRequest {
    decision: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TagRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchTagRequest {
    collection_ids: Vec<i64>,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchMetadataRequest {
    collection_ids: Vec<i64>,
    value: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct BatchMutationResponse {
    summary: BatchMutationSummaryResponse,
    items: Vec<BatchMutationItemResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BatchMutationSummaryResponse {
    total: usize,
    completed: usize,
    succeeded: usize,
    unchanged: usize,
    failed: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct BatchMutationItemResponse {
    collection_id: i64,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    collection: Option<CollectionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<BatchMutationErrorResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BatchMutationErrorResponse {
    code: &'static str,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ManualParodyRequest {
    Text(String),
    Detailed(ManualParodyDetails),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManualParodyDetails {
    raw: String,
    canonical: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ManualClassificationRequest {
    Text(String),
    Detailed(ManualClassificationDetails),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManualClassificationDetails {
    top_level: String,
    subcategory: Option<String>,
}

fn decode_manual_metadata(field: MetadataField, value: Value) -> Result<MetadataValue, ApiError> {
    let invalid =
        || ApiError::bad_request("invalid_metadata_value", "metadata value 的型別或內容無效");
    match field {
        MetadataField::Title | MetadataField::Event | MetadataField::Circle => {
            let value = serde_json::from_value::<String>(value).map_err(|_| invalid())?;
            Ok(MetadataValue::Text(nonempty_metadata_text(value)?))
        }
        MetadataField::Authors => {
            let values = serde_json::from_value::<Vec<String>>(value).map_err(|_| invalid())?;
            let values = values
                .into_iter()
                .map(nonempty_metadata_text)
                .collect::<Result<Vec<_>, _>>()?;
            if values.is_empty() {
                return Err(invalid());
            }
            Ok(MetadataValue::Authors(Authors { raw: None, values }))
        }
        MetadataField::Parody => {
            let value =
                serde_json::from_value::<ManualParodyRequest>(value).map_err(|_| invalid())?;
            let (raw, canonical) = match value {
                ManualParodyRequest::Text(value) => {
                    let value = nonempty_metadata_text(value)?;
                    (value.clone(), value)
                }
                ManualParodyRequest::Detailed(value) => {
                    let raw = nonempty_metadata_text(value.raw)?;
                    let canonical = value
                        .canonical
                        .map(nonempty_metadata_text)
                        .transpose()?
                        .unwrap_or_else(|| raw.clone());
                    (raw, canonical)
                }
            };
            Ok(MetadataValue::Parody(Parody {
                raw,
                canonical,
                evidence: "manual".to_owned(),
            }))
        }
        MetadataField::Classification => {
            let value = serde_json::from_value::<ManualClassificationRequest>(value)
                .map_err(|_| invalid())?;
            let (top_level, subcategory) = match value {
                ManualClassificationRequest::Text(value) => (nonempty_metadata_text(value)?, None),
                ManualClassificationRequest::Detailed(value) => (
                    nonempty_metadata_text(value.top_level)?,
                    value
                        .subcategory
                        .map(|value| value.trim().to_owned())
                        .filter(|value| !value.is_empty()),
                ),
            };
            Ok(MetadataValue::Classification(Classification {
                top_level,
                subcategory,
                raw_marker: None,
            }))
        }
        MetadataField::IsDl => Ok(MetadataValue::Boolean(
            serde_json::from_value::<bool>(value).map_err(|_| invalid())?,
        )),
    }
}

fn nonempty_metadata_text(value: String) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        Err(ApiError::bad_request(
            "invalid_metadata_value",
            "metadata value 不得為空白；清除手動值請使用 DELETE",
        ))
    } else {
        Ok(value.to_owned())
    }
}

pub(crate) async fn decide_metadata_assertion<R>(
    State(state): State<HttpState<R>>,
    Path((collection_id, field, assertion_id)): Path<(String, String, String)>,
    payload: Result<Json<MetadataAssertionDecisionRequest>, JsonRejection>,
) -> Result<Json<MetadataHistoryResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let field = parse_metadata_field(&field)?;
    let assertion_id = parse_metadata_assertion_id(&assertion_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let decision = match payload.decision.as_str() {
        "select" => MetadataAssertionDecision::Select,
        "reject" => MetadataAssertionDecision::Reject,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_metadata_assertion_decision",
                "metadata assertion decision 必須是 select 或 reject",
            ));
        }
    };
    let history = tokio::task::spawn_blocking(move || {
        let mut application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .decide_metadata_assertion(collection_id, field, assertion_id, decision)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(history.try_into()?))
}

pub(crate) async fn set_manual_metadata<R>(
    State(state): State<HttpState<R>>,
    Path((collection_id, field)): Path<(String, String)>,
    payload: Result<Json<ManualMetadataRequest>, JsonRejection>,
) -> Result<Json<CollectionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let field = parse_metadata_field(&field)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let value = decode_manual_metadata(field, payload.value)?;
    let collection = tokio::task::spawn_blocking(move || {
        let mut application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .set_manual_metadata(collection_id, field, value)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(collection.into()))
}

pub(crate) async fn clear_manual_metadata<R>(
    State(state): State<HttpState<R>>,
    Path((collection_id, field)): Path<(String, String)>,
) -> Result<Json<CollectionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let field = parse_metadata_field(&field)?;
    let collection = tokio::task::spawn_blocking(move || {
        let mut application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .clear_manual_metadata(collection_id, field)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(collection.into()))
}

pub(crate) async fn add_collection_tag<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    payload: Result<Json<TagRequest>, JsonRejection>,
) -> Result<Json<CollectionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection = tokio::task::spawn_blocking(move || {
        let mut application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .add_collection_tag(collection_id, &payload.name)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(collection.into()))
}

pub(crate) async fn batch_add_collection_tag<R>(
    State(state): State<HttpState<R>>,
    payload: Result<Json<BatchTagRequest>, JsonRejection>,
) -> Result<Json<BatchMutationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validate_batch_collection_ids(payload.collection_ids)?;
    let name = payload.name.trim().to_owned();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_tag_name",
            "tag name 不得為空白",
        ));
    }
    let report = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        Ok::<_, ApiError>(application.batch_add_collection_tag(&collection_ids, &name))
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(batch_mutation_response(report)))
}

pub(crate) async fn batch_set_manual_metadata<R>(
    State(state): State<HttpState<R>>,
    Path(field): Path<String>,
    payload: Result<Json<BatchMetadataRequest>, JsonRejection>,
) -> Result<Json<BatchMutationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let field = parse_metadata_field(&field)?;
    if !matches!(field, MetadataField::Parody | MetadataField::Classification) {
        return Err(ApiError::bad_request(
            "unsupported_batch_metadata_field",
            "批次 metadata 目前只支援 parody 與 classification",
        ));
    }
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection_ids = validate_batch_collection_ids(payload.collection_ids)?;
    let value = decode_manual_metadata(field, payload.value)?;
    let report = tokio::task::spawn_blocking(move || {
        let mut application = lock_interactive_application(&state.application)?;
        Ok::<_, ApiError>(application.batch_set_manual_metadata(&collection_ids, field, value))
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(batch_mutation_response(report)))
}

pub(crate) fn validate_batch_collection_ids(
    collection_ids: Vec<i64>,
) -> Result<Vec<i64>, ApiError> {
    if collection_ids.is_empty() || collection_ids.len() > 1_000 {
        return Err(ApiError::bad_request(
            "invalid_batch_collection_ids",
            "collection_ids 必須包含 1 到 1000 個 ID",
        ));
    }
    if collection_ids
        .iter()
        .any(|collection_id| *collection_id <= 0)
    {
        return Err(ApiError::bad_request(
            "invalid_batch_collection_ids",
            "collection_ids 必須全部是正整數",
        ));
    }
    let mut seen = HashSet::new();
    Ok(collection_ids
        .into_iter()
        .filter(|collection_id| seen.insert(*collection_id))
        .collect())
}

pub(crate) fn batch_mutation_response(report: ApplicationBatchReport) -> BatchMutationResponse {
    let mut succeeded = 0;
    let mut unchanged = 0;
    let mut failed = 0;
    let items = report
        .items
        .into_iter()
        .map(|item| match item.outcome {
            ApplicationBatchOutcome::Succeeded(collection) => {
                succeeded += 1;
                BatchMutationItemResponse {
                    collection_id: item.collection_id,
                    status: "succeeded",
                    collection: Some(collection.into()),
                    error: None,
                }
            }
            ApplicationBatchOutcome::Unchanged(collection) => {
                unchanged += 1;
                BatchMutationItemResponse {
                    collection_id: item.collection_id,
                    status: "unchanged",
                    collection: Some(collection.into()),
                    error: None,
                }
            }
            ApplicationBatchOutcome::Failed(error) => {
                failed += 1;
                let error = ApiError::from_application(error);
                BatchMutationItemResponse {
                    collection_id: item.collection_id,
                    status: "failed",
                    collection: None,
                    error: Some(BatchMutationErrorResponse {
                        code: error.code,
                        message: error.message,
                    }),
                }
            }
        })
        .collect::<Vec<_>>();
    BatchMutationResponse {
        summary: BatchMutationSummaryResponse {
            total: items.len(),
            completed: items.len(),
            succeeded,
            unchanged,
            failed,
        },
        items,
    }
}

pub(crate) async fn remove_collection_tag<R>(
    State(state): State<HttpState<R>>,
    Path(collection_id): Path<String>,
    payload: Result<Json<TagRequest>, JsonRejection>,
) -> Result<Json<CollectionResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let collection_id = parse_collection_id(&collection_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let collection = tokio::task::spawn_blocking(move || {
        let mut application = match state.application.try_lock() {
            Ok(application) => application,
            Err(TryLockError::WouldBlock) => {
                return Err(ApiError::unavailable(
                    "application_busy",
                    "application service 正在處理其他要求",
                ));
            }
            Err(TryLockError::Poisoned(_)) => return Err(ApiError::internal()),
        };
        application
            .remove_collection_tag(collection_id, &payload.name)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(collection.into()))
}
