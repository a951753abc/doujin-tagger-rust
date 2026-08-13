//! Tombstone identity-consolidation preflight and transaction.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::lifecycle::CollectionStatus;
use crate::metadata::{MetadataField, MetadataSource};
use crate::{
    CatalogRepository, StorageError, StorageResult, rebuild_projection, reselect_by_priority,
    select_assertion,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationBlocker {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualSelectionEvidence {
    pub assertion_id: i64,
    pub source: MetadataSource,
    pub value_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationConflict {
    pub field: MetadataField,
    pub tombstone: ManualSelectionEvidence,
    pub candidate: ManualSelectionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationPreflight {
    pub tombstone_collection_id: i64,
    pub candidate_collection_id: i64,
    pub ready: bool,
    pub already_consolidated: bool,
    pub blockers: Vec<ConsolidationBlocker>,
    pub conflicts: Vec<ConsolidationConflict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidationChoice {
    Tombstone,
    Candidate,
}

impl ConsolidationChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tombstone => "tombstone",
            Self::Candidate => "candidate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsolidationResolution {
    pub field: MetadataField,
    pub choice: ConsolidationChoice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationSnapshot {
    pub consolidation_id: i64,
    pub survivor_collection_id: i64,
    pub merged_collection_id: i64,
    pub already_completed: bool,
    pub resolutions_json: String,
    pub consolidated_at: String,
}

#[derive(Debug, Clone)]
struct SelectedAssertion {
    assertion_id: i64,
    source: MetadataSource,
    selected_by: String,
    value_json: String,
}

impl SelectedAssertion {
    fn is_manual_choice(&self) -> bool {
        self.source == MetadataSource::Manual || self.selected_by == "manual"
    }
}

impl CatalogRepository {
    pub fn consolidation_preflight(
        &self,
        tombstone_collection_id: i64,
        candidate_collection_id: i64,
    ) -> StorageResult<ConsolidationPreflight> {
        build_preflight(
            &self.connection,
            tombstone_collection_id,
            candidate_collection_id,
        )
    }

    pub fn consolidate_tombstone_candidate(
        &mut self,
        tombstone_collection_id: i64,
        candidate_collection_id: i64,
        resolutions: &[ConsolidationResolution],
    ) -> StorageResult<ConsolidationSnapshot> {
        let transaction = self.connection.transaction()?;
        if let Some(snapshot) = consolidation_for_merged(&transaction, candidate_collection_id)? {
            if snapshot.survivor_collection_id != tombstone_collection_id {
                return Err(StorageError::InvalidLifecycle(format!(
                    "收藏 {candidate_collection_id} 已合併至另一筆收藏 {}",
                    snapshot.survivor_collection_id
                )));
            }
            transaction.commit()?;
            return Ok(ConsolidationSnapshot {
                already_completed: true,
                ..snapshot
            });
        }

        let preflight = build_preflight(
            &transaction,
            tombstone_collection_id,
            candidate_collection_id,
        )?;
        if !preflight.blockers.is_empty() {
            return Err(StorageError::InvalidLifecycle(
                preflight
                    .blockers
                    .iter()
                    .map(|blocker| blocker.message.as_str())
                    .collect::<Vec<_>>()
                    .join("；"),
            ));
        }
        let resolution_map = validate_resolutions(&preflight.conflicts, resolutions)?;
        let tombstone_selections = selected_assertions(&transaction, tombstone_collection_id)?;
        let candidate_selections = selected_assertions(&transaction, candidate_collection_id)?;
        let resolutions_json = encode_resolutions(
            &preflight.conflicts,
            &resolution_map,
            &tombstone_selections,
            &candidate_selections,
        );

        transaction.execute(
            "INSERT INTO collection_consolidations(
                 survivor_collection_id, merged_collection_id, resolutions_json
             ) VALUES (?1, ?2, ?3)",
            params![
                tombstone_collection_id,
                candidate_collection_id,
                resolutions_json
            ],
        )?;
        let consolidation_id = transaction.last_insert_rowid();
        record_transfers(&transaction, consolidation_id, candidate_collection_id)?;

        transaction.execute(
            "DELETE FROM metadata_selections WHERE collection_id IN (?1, ?2)",
            params![tombstone_collection_id, candidate_collection_id],
        )?;
        transaction.execute(
            "DELETE FROM effective_metadata WHERE collection_id = ?1",
            [candidate_collection_id],
        )?;
        transfer_records(
            &transaction,
            tombstone_collection_id,
            candidate_collection_id,
        )?;
        crate::work_baskets::transfer_work_basket_memberships(
            &transaction,
            tombstone_collection_id,
            candidate_collection_id,
        )?;

        for field in MetadataField::ALL {
            reselect_by_priority(&transaction, tombstone_collection_id, field, false)?;
        }
        preserve_tombstone_selections(
            &transaction,
            tombstone_collection_id,
            &tombstone_selections,
            &candidate_selections,
        )?;
        apply_resolutions(
            &transaction,
            tombstone_collection_id,
            &preflight.conflicts,
            &resolution_map,
        )?;
        rebuild_projection(&transaction, tombstone_collection_id)?;

        transaction.execute(
            "UPDATE collections
             SET status = 'active', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [tombstone_collection_id],
        )?;
        transaction.execute(
            "UPDATE collections
             SET status = 'tombstone', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [candidate_collection_id],
        )?;

        let snapshot = consolidation_for_merged(&transaction, candidate_collection_id)?
            .ok_or_else(|| {
                StorageError::InvalidLifecycle(
                    "consolidation 完成後無法讀回 audit record".to_owned(),
                )
            })?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn merged_into_collection(&self, collection_id: i64) -> StorageResult<Option<i64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT survivor_collection_id FROM collection_consolidations
                 WHERE merged_collection_id = ?1",
                [collection_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn consolidation_transfer_count(&self, consolidation_id: i64) -> StorageResult<i64> {
        Ok(self.connection.query_row(
            "SELECT count(*) FROM collection_consolidation_transfers
             WHERE consolidation_id = ?1",
            [consolidation_id],
            |row| row.get(0),
        )?)
    }
}

fn build_preflight(
    connection: &Connection,
    tombstone_collection_id: i64,
    candidate_collection_id: i64,
) -> StorageResult<ConsolidationPreflight> {
    if let Some(snapshot) = consolidation_for_merged(connection, candidate_collection_id)? {
        let same_survivor = snapshot.survivor_collection_id == tombstone_collection_id;
        return Ok(ConsolidationPreflight {
            tombstone_collection_id,
            candidate_collection_id,
            ready: same_survivor,
            already_consolidated: same_survivor,
            blockers: if same_survivor {
                Vec::new()
            } else {
                vec![blocker(
                    "already_consolidated",
                    format!("候選已合併至收藏 {}", snapshot.survivor_collection_id),
                )]
            },
            conflicts: Vec::new(),
        });
    }

    let mut blockers = Vec::new();
    let tombstone_status = collection_status(connection, tombstone_collection_id)?;
    let candidate_status = collection_status(connection, candidate_collection_id)?;
    if tombstone_status != Some(CollectionStatus::Tombstone) {
        blockers.push(blocker(
            "survivor_not_tombstone",
            "survivor 必須是 tombstone 收藏".to_owned(),
        ));
    }
    if candidate_status != Some(CollectionStatus::Active) {
        blockers.push(blocker(
            "candidate_not_active",
            "candidate 必須是 active 收藏".to_owned(),
        ));
    }

    let decisions = candidate_decisions(connection, tombstone_collection_id)?;
    let relation = decisions
        .iter()
        .find(|(candidate_id, _)| *candidate_id == candidate_collection_id);
    if relation.is_none() {
        blockers.push(blocker(
            "candidate_link_missing",
            "找不到指定的 tombstone candidate 關聯".to_owned(),
        ));
    } else if !matches!(relation, Some((_, decision)) if decision == "confirmed") {
        blockers.push(blocker(
            "candidate_not_confirmed",
            "指定 candidate 尚未 confirmed".to_owned(),
        ));
    }
    let pending = decisions
        .iter()
        .filter(|(_, decision)| decision == "pending")
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        blockers.push(blocker(
            "pending_candidates",
            format!("仍有 pending candidates：{pending:?}"),
        ));
    }
    let confirmed = decisions
        .iter()
        .filter(|(_, decision)| decision == "confirmed")
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    if confirmed != [candidate_collection_id] {
        blockers.push(blocker(
            "confirmed_candidate_count",
            format!("必須恰有指定的一筆 confirmed candidate，目前為：{confirmed:?}"),
        ));
    }
    let active_jobs: i64 = connection.query_row(
        "SELECT count(*) FROM background_jobs
         WHERE collection_id IN (?1, ?2) AND status IN ('pending', 'running')",
        params![tombstone_collection_id, candidate_collection_id],
        |row| row.get(0),
    )?;
    if active_jobs > 0 {
        blockers.push(blocker(
            "active_background_jobs",
            "雙方仍有 pending 或 running background job".to_owned(),
        ));
    }

    let conflicts = manual_conflicts(connection, tombstone_collection_id, candidate_collection_id)?;
    Ok(ConsolidationPreflight {
        tombstone_collection_id,
        candidate_collection_id,
        ready: blockers.is_empty() && conflicts.is_empty(),
        already_consolidated: false,
        blockers,
        conflicts,
    })
}

fn collection_status(
    connection: &Connection,
    collection_id: i64,
) -> StorageResult<Option<CollectionStatus>> {
    connection
        .query_row(
            "SELECT status FROM collections WHERE id = ?1",
            [collection_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| CollectionStatus::parse(&value).map_err(StorageError::InvalidSchema))
        .transpose()
}

fn candidate_decisions(
    connection: &Connection,
    tombstone_collection_id: i64,
) -> StorageResult<Vec<(i64, String)>> {
    let mut statement = connection.prepare(
        "SELECT candidate_collection_id, decision
         FROM tombstone_candidates
         WHERE tombstone_collection_id = ?1
         ORDER BY candidate_collection_id",
    )?;
    Ok(statement
        .query_map([tombstone_collection_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn selected_assertions(
    connection: &Connection,
    collection_id: i64,
) -> StorageResult<HashMap<MetadataField, SelectedAssertion>> {
    let mut statement = connection.prepare(
        "SELECT assertion.id, selection.field_name, assertion.source_kind,
                selection.selected_by, assertion.value_json
         FROM metadata_selections AS selection
         JOIN metadata_assertions AS assertion ON assertion.id = selection.assertion_id
         WHERE selection.collection_id = ?1",
    )?;
    let rows = statement
        .query_map([collection_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(assertion_id, field, source, selected_by, value_json)| {
            let field = MetadataField::parse(&field).map_err(StorageError::InvalidSchema)?;
            Ok((
                field,
                SelectedAssertion {
                    assertion_id,
                    source: MetadataSource::parse(&source).map_err(StorageError::InvalidSchema)?,
                    selected_by,
                    value_json,
                },
            ))
        })
        .collect()
}

fn manual_conflicts(
    connection: &Connection,
    tombstone_collection_id: i64,
    candidate_collection_id: i64,
) -> StorageResult<Vec<ConsolidationConflict>> {
    let tombstone = selected_assertions(connection, tombstone_collection_id)?;
    let candidate = selected_assertions(connection, candidate_collection_id)?;
    let mut conflicts = Vec::new();
    for field in MetadataField::ALL {
        let (Some(left), Some(right)) = (tombstone.get(&field), candidate.get(&field)) else {
            continue;
        };
        if left.is_manual_choice()
            && right.is_manual_choice()
            && json_values_differ(&left.value_json, &right.value_json)?
        {
            conflicts.push(ConsolidationConflict {
                field,
                tombstone: manual_evidence(left),
                candidate: manual_evidence(right),
            });
        }
    }
    Ok(conflicts)
}

fn json_values_differ(left: &str, right: &str) -> StorageResult<bool> {
    Ok(serde_json::from_str::<serde_json::Value>(left)?
        != serde_json::from_str::<serde_json::Value>(right)?)
}

fn manual_evidence(assertion: &SelectedAssertion) -> ManualSelectionEvidence {
    ManualSelectionEvidence {
        assertion_id: assertion.assertion_id,
        source: assertion.source,
        value_json: assertion.value_json.clone(),
    }
}

fn validate_resolutions(
    conflicts: &[ConsolidationConflict],
    resolutions: &[ConsolidationResolution],
) -> StorageResult<HashMap<MetadataField, ConsolidationChoice>> {
    let expected = conflicts
        .iter()
        .map(|conflict| conflict.field)
        .collect::<HashSet<_>>();
    let mut resolution_map = HashMap::new();
    for resolution in resolutions {
        if !expected.contains(&resolution.field) {
            return Err(StorageError::InvalidLifecycle(format!(
                "{} 不是這次 consolidation 的手動衝突欄位",
                resolution.field.as_str()
            )));
        }
        if resolution_map
            .insert(resolution.field, resolution.choice)
            .is_some()
        {
            return Err(StorageError::InvalidLifecycle(format!(
                "{} conflict resolution 重複",
                resolution.field.as_str()
            )));
        }
    }
    let missing = expected
        .iter()
        .filter(|field| !resolution_map.contains_key(field))
        .map(|field| field.as_str())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(StorageError::InvalidLifecycle(format!(
            "仍有未裁決的手動 metadata 衝突：{missing:?}"
        )));
    }
    Ok(resolution_map)
}

fn encode_resolutions(
    conflicts: &[ConsolidationConflict],
    resolutions: &HashMap<MetadataField, ConsolidationChoice>,
    tombstone: &HashMap<MetadataField, SelectedAssertion>,
    candidate: &HashMap<MetadataField, SelectedAssertion>,
) -> String {
    serde_json::Value::Array(
        conflicts
            .iter()
            .map(|conflict| {
                let choice = resolutions[&conflict.field];
                let chosen_assertion_id = match choice {
                    ConsolidationChoice::Tombstone => tombstone[&conflict.field].assertion_id,
                    ConsolidationChoice::Candidate => candidate[&conflict.field].assertion_id,
                };
                serde_json::json!({
                    "field": conflict.field.as_str(),
                    "choice": choice.as_str(),
                    "chosen_assertion_id": chosen_assertion_id,
                    "tombstone_assertion_id": conflict.tombstone.assertion_id,
                    "candidate_assertion_id": conflict.candidate.assertion_id,
                })
            })
            .collect(),
    )
    .to_string()
}

fn record_transfers(
    transaction: &Transaction<'_>,
    consolidation_id: i64,
    candidate_collection_id: i64,
) -> StorageResult<()> {
    for (kind, table) in [
        ("location", "collection_locations"),
        ("parser_run", "parser_runs"),
        ("metadata_assertion", "metadata_assertions"),
        ("external_search_result", "external_search_results"),
        ("tag_relation", "collection_tags"),
        ("file_operation", "file_operations"),
    ] {
        let id_column = if table == "collection_tags" {
            "tag_id"
        } else {
            "id"
        };
        let sql = format!(
            "INSERT INTO collection_consolidation_transfers(
                 consolidation_id, record_kind, record_id, original_collection_id
             )
             SELECT ?1, ?2, {id_column}, ?3 FROM {table} WHERE collection_id = ?3"
        );
        transaction.execute(
            &sql,
            params![consolidation_id, kind, candidate_collection_id],
        )?;
    }
    Ok(())
}

fn transfer_records(
    transaction: &Transaction<'_>,
    survivor_collection_id: i64,
    candidate_collection_id: i64,
) -> StorageResult<()> {
    transaction.execute(
        "UPDATE collection_locations SET collection_id = ?1 WHERE collection_id = ?2",
        params![survivor_collection_id, candidate_collection_id],
    )?;
    transaction.execute(
        "UPDATE parser_runs SET collection_id = ?1 WHERE collection_id = ?2",
        params![survivor_collection_id, candidate_collection_id],
    )?;
    transaction.execute(
        "UPDATE metadata_assertions SET collection_id = ?1 WHERE collection_id = ?2",
        params![survivor_collection_id, candidate_collection_id],
    )?;
    transaction.execute(
        "UPDATE external_search_results SET collection_id = ?1 WHERE collection_id = ?2",
        params![survivor_collection_id, candidate_collection_id],
    )?;
    transaction.execute(
        "INSERT INTO collection_tags(collection_id, tag_id)
         SELECT ?1, tag_id FROM collection_tags WHERE collection_id = ?2
         ON CONFLICT(collection_id, tag_id) DO NOTHING",
        params![survivor_collection_id, candidate_collection_id],
    )?;
    transaction.execute(
        "DELETE FROM collection_tags WHERE collection_id = ?1",
        [candidate_collection_id],
    )?;
    transaction.execute(
        "UPDATE file_operations SET collection_id = ?1 WHERE collection_id = ?2",
        params![survivor_collection_id, candidate_collection_id],
    )?;
    Ok(())
}

fn preserve_tombstone_selections(
    transaction: &Transaction<'_>,
    survivor_collection_id: i64,
    tombstone: &HashMap<MetadataField, SelectedAssertion>,
    candidate: &HashMap<MetadataField, SelectedAssertion>,
) -> StorageResult<()> {
    for field in MetadataField::ALL {
        let Some(left) = tombstone.get(&field) else {
            continue;
        };
        let preserve = match candidate.get(&field) {
            None => true,
            Some(right) if !right.is_manual_choice() => true,
            Some(right) if left.is_manual_choice() => {
                !json_values_differ(&left.value_json, &right.value_json)?
            }
            Some(_) => false,
        };
        if preserve {
            select_assertion(
                transaction,
                survivor_collection_id,
                field,
                left.assertion_id,
                &left.selected_by,
            )?;
        }
    }
    Ok(())
}

fn apply_resolutions(
    transaction: &Transaction<'_>,
    survivor_collection_id: i64,
    conflicts: &[ConsolidationConflict],
    resolutions: &HashMap<MetadataField, ConsolidationChoice>,
) -> StorageResult<()> {
    for conflict in conflicts {
        let assertion_id = match resolutions[&conflict.field] {
            ConsolidationChoice::Tombstone => conflict.tombstone.assertion_id,
            ConsolidationChoice::Candidate => conflict.candidate.assertion_id,
        };
        select_assertion(
            transaction,
            survivor_collection_id,
            conflict.field,
            assertion_id,
            "manual",
        )?;
    }
    Ok(())
}

fn consolidation_for_merged(
    connection: &Connection,
    merged_collection_id: i64,
) -> StorageResult<Option<ConsolidationSnapshot>> {
    Ok(connection
        .query_row(
            "SELECT id, survivor_collection_id, merged_collection_id,
                    resolutions_json, consolidated_at
             FROM collection_consolidations WHERE merged_collection_id = ?1",
            [merged_collection_id],
            |row| {
                Ok(ConsolidationSnapshot {
                    consolidation_id: row.get(0)?,
                    survivor_collection_id: row.get(1)?,
                    merged_collection_id: row.get(2)?,
                    already_completed: false,
                    resolutions_json: row.get(3)?,
                    consolidated_at: row.get(4)?,
                })
            },
        )
        .optional()?)
}

fn blocker(kind: &str, message: String) -> ConsolidationBlocker {
    ConsolidationBlocker {
        kind: kind.to_owned(),
        message,
    }
}
