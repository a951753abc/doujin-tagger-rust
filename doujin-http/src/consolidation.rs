//! Tombstone candidate and consolidation endpoints.

use std::sync::TryLockError;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use doujin_files::RecycleBin;
use doujin_storage::consolidation::{
    ConsolidationChoice, ConsolidationConflict, ConsolidationPreflight, ConsolidationResolution,
    ConsolidationSnapshot, ManualSelectionEvidence,
};
use doujin_storage::lifecycle::{CandidateDecision, TombstoneCandidateSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ApiError;
use crate::params::{
    candidate_decision_name, parse_candidate_id, parse_metadata_field, parse_tombstone_id,
};
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TombstoneCandidateDecisionRequest {
    decision: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct TombstoneCandidatesResponse {
    items: Vec<TombstoneCandidateResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TombstoneCandidateResponse {
    tombstone_collection_id: i64,
    candidate_collection_id: i64,
    tombstone_path: String,
    candidate_path: Option<String>,
    reason: String,
    decision: &'static str,
    discovered_at: String,
    decided_at: Option<String>,
}

impl From<TombstoneCandidateSnapshot> for TombstoneCandidateResponse {
    fn from(candidate: TombstoneCandidateSnapshot) -> Self {
        Self {
            tombstone_collection_id: candidate.tombstone_collection_id,
            candidate_collection_id: candidate.candidate_collection_id,
            tombstone_path: candidate.tombstone_path.to_string_lossy().into_owned(),
            candidate_path: candidate
                .candidate_path
                .map(|path| path.to_string_lossy().into_owned()),
            reason: candidate.reason,
            decision: candidate_decision_name(candidate.decision),
            discovered_at: candidate.discovered_at,
            decided_at: candidate.decided_at,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConsolidationRequest {
    #[serde(default)]
    resolutions: Vec<ConsolidationResolutionRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConsolidationResolutionRequest {
    field: String,
    choice: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ConsolidationPreflightResponse {
    tombstone_collection_id: i64,
    candidate_collection_id: i64,
    ready: bool,
    already_consolidated: bool,
    blockers: Vec<ConsolidationBlockerResponse>,
    conflicts: Vec<ConsolidationConflictResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ConsolidationBlockerResponse {
    kind: String,
    message: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ConsolidationConflictResponse {
    field: &'static str,
    tombstone: ManualSelectionEvidenceResponse,
    candidate: ManualSelectionEvidenceResponse,
}

#[derive(Debug, Serialize)]
pub(crate) struct ManualSelectionEvidenceResponse {
    assertion_id: i64,
    source: &'static str,
    value: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct ConsolidationResponse {
    consolidation_id: i64,
    survivor_collection_id: i64,
    merged_collection_id: i64,
    already_completed: bool,
    resolutions: Value,
    consolidated_at: String,
}

impl TryFrom<ConsolidationPreflight> for ConsolidationPreflightResponse {
    type Error = ApiError;

    fn try_from(preflight: ConsolidationPreflight) -> Result<Self, Self::Error> {
        Ok(Self {
            tombstone_collection_id: preflight.tombstone_collection_id,
            candidate_collection_id: preflight.candidate_collection_id,
            ready: preflight.ready,
            already_consolidated: preflight.already_consolidated,
            blockers: preflight
                .blockers
                .into_iter()
                .map(|blocker| ConsolidationBlockerResponse {
                    kind: blocker.kind,
                    message: blocker.message,
                })
                .collect(),
            conflicts: preflight
                .conflicts
                .into_iter()
                .map(consolidation_conflict_response)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TryFrom<ConsolidationSnapshot> for ConsolidationResponse {
    type Error = ApiError;

    fn try_from(snapshot: ConsolidationSnapshot) -> Result<Self, Self::Error> {
        Ok(Self {
            consolidation_id: snapshot.consolidation_id,
            survivor_collection_id: snapshot.survivor_collection_id,
            merged_collection_id: snapshot.merged_collection_id,
            already_completed: snapshot.already_completed,
            resolutions: serde_json::from_str(&snapshot.resolutions_json)
                .map_err(|_| ApiError::internal())?,
            consolidated_at: snapshot.consolidated_at,
        })
    }
}

pub(crate) fn consolidation_conflict_response(
    conflict: ConsolidationConflict,
) -> Result<ConsolidationConflictResponse, ApiError> {
    Ok(ConsolidationConflictResponse {
        field: conflict.field.as_str(),
        tombstone: manual_selection_evidence_response(conflict.tombstone)?,
        candidate: manual_selection_evidence_response(conflict.candidate)?,
    })
}

pub(crate) fn manual_selection_evidence_response(
    evidence: ManualSelectionEvidence,
) -> Result<ManualSelectionEvidenceResponse, ApiError> {
    Ok(ManualSelectionEvidenceResponse {
        assertion_id: evidence.assertion_id,
        source: evidence.source.as_str(),
        value: serde_json::from_str(&evidence.value_json).map_err(|_| ApiError::internal())?,
    })
}

pub(crate) async fn list_tombstone_candidates<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<TombstoneCandidatesResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let candidates = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .tombstone_candidates()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(TombstoneCandidatesResponse {
        items: candidates.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn decide_tombstone_candidate<R>(
    State(state): State<HttpState<R>>,
    Path((tombstone_id, candidate_id)): Path<(String, String)>,
    payload: Result<Json<TombstoneCandidateDecisionRequest>, JsonRejection>,
) -> Result<Json<TombstoneCandidateResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let tombstone_id = parse_tombstone_id(&tombstone_id)?;
    let candidate_id = parse_candidate_id(&candidate_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let decision = match payload.decision.as_str() {
        "confirmed" => CandidateDecision::Confirmed,
        "rejected" => CandidateDecision::Rejected,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_tombstone_candidate_decision",
                "tombstone candidate decision 必須是 confirmed 或 rejected",
            ));
        }
    };
    let candidate = tokio::task::spawn_blocking(move || {
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
            .decide_tombstone_candidate(tombstone_id, candidate_id, decision)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(candidate.into()))
}

pub(crate) async fn consolidation_preflight<R>(
    State(state): State<HttpState<R>>,
    Path((tombstone_id, candidate_id)): Path<(String, String)>,
) -> Result<Json<ConsolidationPreflightResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let tombstone_id = parse_tombstone_id(&tombstone_id)?;
    let candidate_id = parse_candidate_id(&candidate_id)?;
    let preflight = tokio::task::spawn_blocking(move || {
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
            .consolidation_preflight(tombstone_id, candidate_id)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(preflight.try_into()?))
}

pub(crate) async fn consolidate_tombstone_candidate<R>(
    State(state): State<HttpState<R>>,
    Path((tombstone_id, candidate_id)): Path<(String, String)>,
    payload: Result<Json<ConsolidationRequest>, JsonRejection>,
) -> Result<Json<ConsolidationResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let tombstone_id = parse_tombstone_id(&tombstone_id)?;
    let candidate_id = parse_candidate_id(&candidate_id)?;
    let Json(payload) =
        payload.map_err(|_| ApiError::bad_request("invalid_json", "JSON request body 無效"))?;
    let resolutions = payload
        .resolutions
        .into_iter()
        .map(parse_consolidation_resolution)
        .collect::<Result<Vec<_>, _>>()?;
    let consolidated = tokio::task::spawn_blocking(move || {
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
            .consolidate_tombstone_candidate(tombstone_id, candidate_id, &resolutions)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(consolidated.try_into()?))
}

pub(crate) fn parse_consolidation_resolution(
    resolution: ConsolidationResolutionRequest,
) -> Result<ConsolidationResolution, ApiError> {
    let field = parse_metadata_field(&resolution.field).map_err(|_| {
        ApiError::bad_request(
            "invalid_consolidation_resolution",
            "consolidation resolution 包含不支援的 metadata field",
        )
    })?;
    let choice = match resolution.choice.as_str() {
        "tombstone" => ConsolidationChoice::Tombstone,
        "candidate" => ConsolidationChoice::Candidate,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_consolidation_resolution",
                "consolidation choice 必須是 tombstone 或 candidate",
            ));
        }
    };
    Ok(ConsolidationResolution { field, choice })
}
