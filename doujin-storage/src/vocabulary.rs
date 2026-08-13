//! Metadata vocabulary candidate detection and user-confirmed governance.
//!
//! This module deliberately operates on metadata assertions and canonical entities. It does not
//! share tables or concepts with collection tombstones and identity consolidation.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::canonical::EntityKind;
use crate::{CatalogRepository, StorageError, StorageResult, rebuild_projection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VocabularyField {
    Event,
    Circle,
    Author,
    Parody,
}

impl VocabularyField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Circle => "circle",
            Self::Author => "author",
            Self::Parody => "parody",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "event" => Ok(Self::Event),
            "circle" => Ok(Self::Circle),
            "author" => Ok(Self::Author),
            "parody" => Ok(Self::Parody),
            _ => Err(format!("名稱治理不支援欄位：{value}")),
        }
    }

    fn assertion_field(self) -> &'static str {
        match self {
            Self::Author => "authors",
            _ => self.as_str(),
        }
    }

    fn entity_kind(self) -> EntityKind {
        match self {
            Self::Event => EntityKind::Event,
            Self::Circle => EntityKind::Circle,
            Self::Author => EntityKind::Author,
            Self::Parody => EntityKind::Parody,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VocabularyRepresentative {
    pub collection_id: i64,
    pub title: Option<String>,
    pub filename: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VocabularySourceCount {
    pub source: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VocabularyVariant {
    pub value: String,
    pub normalized: String,
    pub active_count: i64,
    pub source_counts: Vec<VocabularySourceCount>,
    pub representatives: Vec<VocabularyRepresentative>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VocabularyCandidateGroup {
    pub id: String,
    pub field: VocabularyField,
    pub variants: Vec<VocabularyVariant>,
    pub suggested_canonical: String,
    pub suggestion_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VocabularySavedViewImpact {
    pub id: i64,
    pub name: String,
    pub previous_value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VocabularyMergePreflight {
    pub field: VocabularyField,
    pub canonical: String,
    pub variants: Vec<String>,
    pub affected_collections: i64,
    pub source_counts: Vec<VocabularySourceCount>,
    pub manual_assertions: i64,
    pub manual_selected_conflicts: i64,
    pub saved_views: Vec<VocabularySavedViewImpact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VocabularyMergeResult {
    pub entity_id: i64,
    pub canonical: String,
    pub affected_collections: i64,
    pub saved_views_updated: i64,
}

#[derive(Debug, Clone)]
struct SelectedValue {
    collection_id: i64,
    assertion_id: i64,
    value_index: usize,
    value: String,
    source: String,
    selected_by: String,
    title: Option<String>,
    filename: String,
}

#[derive(Default)]
struct VariantAccumulator {
    collections: BTreeSet<i64>,
    sources: BTreeMap<String, BTreeSet<i64>>,
    representatives: Vec<VocabularyRepresentative>,
}

impl CatalogRepository {
    pub fn vocabulary_candidates(
        &self,
        field: Option<VocabularyField>,
    ) -> StorageResult<Vec<VocabularyCandidateGroup>> {
        let fields = field.map_or_else(
            || {
                vec![
                    VocabularyField::Event,
                    VocabularyField::Circle,
                    VocabularyField::Author,
                    VocabularyField::Parody,
                ]
            },
            |field| vec![field],
        );
        let exclusions = load_exclusions(&self.connection)?;
        let aliases = load_aliases(&self.connection)?;
        let mut groups = Vec::new();

        for field in fields {
            let rows = selected_values(&self.connection, field)?;
            let mut normalized_groups: BTreeMap<String, BTreeMap<String, VariantAccumulator>> =
                BTreeMap::new();
            for row in rows {
                let normalized = normalize_vocabulary_name(&row.value);
                if normalized.is_empty() {
                    continue;
                }
                let accumulator = normalized_groups
                    .entry(normalized)
                    .or_default()
                    .entry(row.value.clone())
                    .or_default();
                accumulator.collections.insert(row.collection_id);
                accumulator
                    .sources
                    .entry(row.source)
                    .or_default()
                    .insert(row.collection_id);
                if accumulator.representatives.len() < 3
                    && !accumulator
                        .representatives
                        .iter()
                        .any(|item| item.collection_id == row.collection_id)
                {
                    accumulator.representatives.push(VocabularyRepresentative {
                        collection_id: row.collection_id,
                        title: row.title,
                        filename: row.filename,
                    });
                }
            }

            for (normalized, variants) in normalized_groups {
                if variants.len() < 2 {
                    continue;
                }
                let components = candidate_components(field, variants.keys(), &exclusions);
                for component in components
                    .into_iter()
                    .filter(|component| component.len() > 1)
                {
                    let mapped_entities = component
                        .iter()
                        .filter_map(|value| aliases.get(&(field, value.clone())).copied())
                        .collect::<BTreeSet<_>>();
                    if mapped_entities.len() == 1
                        && component
                            .iter()
                            .all(|value| aliases.contains_key(&(field, value.clone())))
                    {
                        continue;
                    }
                    let mut group_variants = component
                        .iter()
                        .map(|value| {
                            let aggregate = &variants[value];
                            VocabularyVariant {
                                value: value.clone(),
                                normalized: normalized.clone(),
                                active_count: aggregate.collections.len() as i64,
                                source_counts: aggregate
                                    .sources
                                    .iter()
                                    .map(|(source, collections)| VocabularySourceCount {
                                        source: source.clone(),
                                        count: collections.len() as i64,
                                    })
                                    .collect(),
                                representatives: aggregate.representatives.clone(),
                            }
                        })
                        .collect::<Vec<_>>();
                    group_variants.sort_by(|left, right| {
                        right
                            .active_count
                            .cmp(&left.active_count)
                            .then_with(|| left.value.cmp(&right.value))
                    });
                    let (suggested_canonical, suggestion_reason) = suggested_canonical(
                        &self.connection,
                        field,
                        &group_variants,
                        &mapped_entities,
                    )?;
                    groups.push(VocabularyCandidateGroup {
                        id: format!("{}:{}", field.as_str(), normalized),
                        field,
                        variants: group_variants,
                        suggested_canonical,
                        suggestion_reason,
                    });
                }
            }
        }

        groups.sort_by(|left, right| {
            left.field
                .cmp(&right.field)
                .then_with(|| {
                    let left_count = left
                        .variants
                        .iter()
                        .map(|variant| variant.active_count)
                        .sum::<i64>();
                    let right_count = right
                        .variants
                        .iter()
                        .map(|variant| variant.active_count)
                        .sum::<i64>();
                    right_count.cmp(&left_count)
                })
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(groups)
    }

    pub fn vocabulary_merge_preflight(
        &self,
        field: VocabularyField,
        canonical: &str,
        variants: &[String],
    ) -> StorageResult<VocabularyMergePreflight> {
        vocabulary_merge_preflight(&self.connection, field, canonical, variants)
    }

    pub fn merge_vocabulary(
        &mut self,
        field: VocabularyField,
        canonical: &str,
        variants: &[String],
    ) -> StorageResult<VocabularyMergeResult> {
        let transaction = self.connection.transaction()?;
        let preflight = vocabulary_merge_preflight(&transaction, field, canonical, variants)?;
        let entity_id = ensure_vocabulary_entity(&transaction, field, &preflight.canonical)?;
        let evidence_json = serde_json::json!({
            "source_reference": "vocabulary_governance",
            "reason": "user_confirmed_canonical_merge",
        })
        .to_string();

        for variant in &preflight.variants {
            transaction.execute(
                "INSERT INTO name_variants(entity_id, raw_name, source_kind, evidence_json)
                 VALUES (?1, ?2, 'manual', ?3)
                 ON CONFLICT(entity_id, raw_name) DO UPDATE SET evidence_json = excluded.evidence_json",
                params![entity_id, variant, evidence_json],
            )?;
            transaction.execute(
                "INSERT INTO vocabulary_aliases(field_name, alias, entity_id, source)
                 VALUES (?1, ?2, ?3, 'user_confirmed')
                 ON CONFLICT(field_name, alias) DO UPDATE SET entity_id = excluded.entity_id,
                     source = excluded.source, created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                params![field.as_str(), variant, entity_id],
            )?;
        }

        let selected = selected_values(&transaction, field)?;
        let variant_set = preflight.variants.iter().collect::<HashSet<_>>();
        let mut affected = BTreeSet::new();
        for value in selected
            .iter()
            .filter(|value| variant_set.contains(&value.value))
        {
            transaction.execute(
                "INSERT INTO assertion_entities(assertion_id, entity_id, value_index, raw_name, evidence_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(assertion_id, value_index) DO UPDATE SET entity_id = excluded.entity_id,
                     raw_name = excluded.raw_name, evidence_json = excluded.evidence_json",
                params![value.assertion_id, entity_id, value.value_index as i64, value.value, evidence_json],
            )?;
            affected.insert(value.collection_id);
        }

        let saved_views_updated =
            rewrite_saved_views(&transaction, field, &preflight.canonical, &variant_set)?;
        for collection_id in &affected {
            rebuild_projection(&transaction, *collection_id)?;
        }
        transaction.commit()?;
        Ok(VocabularyMergeResult {
            entity_id,
            canonical: preflight.canonical,
            affected_collections: affected.len() as i64,
            saved_views_updated,
        })
    }

    pub fn reject_vocabulary_group(
        &mut self,
        field: VocabularyField,
        values: &[String],
        reason: &str,
        removed: bool,
    ) -> StorageResult<usize> {
        let values = validated_values(values)?;
        if values.len() < 2 || reason.trim().is_empty() {
            return Err(StorageError::InvalidCanonicalMapping(
                "拒絕名稱候選必須包含至少兩個名稱與理由".to_owned(),
            ));
        }
        let transaction = self.connection.transaction()?;
        let mut changed = 0;
        for left_index in 0..values.len() {
            for right_index in (left_index + 1)..values.len() {
                let (left, right) = ordered_pair(&values[left_index], &values[right_index]);
                changed += transaction.execute(
                    "INSERT INTO vocabulary_exclusions(field_name, left_value, right_value, reason, source)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(field_name, left_value, right_value) DO UPDATE SET
                         reason = excluded.reason, source = excluded.source,
                         created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                    params![field.as_str(), left, right, reason.trim(), if removed { "user_removed" } else { "user_rejected" }],
                )?;
            }
        }
        transaction.commit()?;
        Ok(changed)
    }
}

pub fn normalize_vocabulary_name(value: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in value.nfkc().flat_map(char::to_lowercase) {
        let character = katakana_to_hiragana(character);
        if character.is_whitespace() || is_safe_separator(character) {
            pending_space = !normalized.is_empty();
            continue;
        }
        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }
        normalized.push(character);
    }
    normalized
}

fn katakana_to_hiragana(character: char) -> char {
    if ('ァ'..='ヶ').contains(&character) {
        char::from_u32(character as u32 - 0x60).unwrap_or(character)
    } else {
        character
    }
}

fn is_safe_separator(character: char) -> bool {
    matches!(
        character,
        '・' | '･'
            | '·'
            | '、'
            | '，'
            | ','
            | '。'
            | '．'
            | '.'
            | '：'
            | ':'
            | '；'
            | ';'
            | '／'
            | '/'
            | '\\'
            | '｜'
            | '|'
            | '‐'
            | '‑'
            | '‒'
            | '–'
            | '—'
            | '―'
            | '-'
            | '_'
            | '「'
            | '」'
            | '『'
            | '』'
            | '【'
            | '】'
            | '['
            | ']'
            | '('
            | ')'
            | '（'
            | '）'
    )
}

fn selected_values(
    connection: &Connection,
    field: VocabularyField,
) -> StorageResult<Vec<SelectedValue>> {
    let mut statement = connection.prepare(
        "SELECT collection.id, assertion.id, assertion.value_json, assertion.source_kind,
                selection.selected_by, metadata.title, location.filename
         FROM metadata_selections AS selection
         JOIN metadata_assertions AS assertion ON assertion.id = selection.assertion_id
         JOIN collections AS collection ON collection.id = selection.collection_id
         JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
         JOIN collection_locations AS location ON location.id = (
             SELECT current_location.id FROM collection_locations AS current_location
             WHERE current_location.collection_id = collection.id
               AND current_location.location_status = 'current'
             ORDER BY current_location.id DESC LIMIT 1
         )
         WHERE collection.status = 'active' AND selection.field_name = ?1
         ORDER BY collection.id, assertion.id",
    )?;
    let raw_rows = statement
        .query_map([field.assertion_field()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut values = Vec::new();
    for (collection_id, assertion_id, value_json, source, selected_by, title, filename) in raw_rows
    {
        for (value_index, value) in values_from_json(field, &value_json)? {
            values.push(SelectedValue {
                collection_id,
                assertion_id,
                value_index,
                value,
                source: source.clone(),
                selected_by: selected_by.clone(),
                title: title.clone(),
                filename: filename.clone(),
            });
        }
    }
    Ok(values)
}

fn values_from_json(
    field: VocabularyField,
    value_json: &str,
) -> StorageResult<Vec<(usize, String)>> {
    let value: Value = serde_json::from_str(value_json)?;
    let values = match field {
        VocabularyField::Event | VocabularyField::Circle => {
            value.as_str().map(|value| vec![(0, value.to_owned())])
        }
        VocabularyField::Author => value.get("values").and_then(Value::as_array).map(|values| {
            values
                .iter()
                .enumerate()
                .filter_map(|(index, value)| value.as_str().map(|value| (index, value.to_owned())))
                .collect()
        }),
        VocabularyField::Parody => value
            .get("canonical")
            .and_then(Value::as_str)
            .map(|value| vec![(0, value.to_owned())]),
    }
    .ok_or_else(|| {
        StorageError::InvalidSchema(format!("{} assertion JSON 無效", field.as_str()))
    })?;
    Ok(values
        .into_iter()
        .filter(|(_, value)| !value.trim().is_empty())
        .collect())
}

fn load_exclusions(
    connection: &Connection,
) -> StorageResult<HashSet<(VocabularyField, String, String)>> {
    let mut statement = connection
        .prepare("SELECT field_name, left_value, right_value FROM vocabulary_exclusions")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(field, left, right)| {
            Ok((
                VocabularyField::parse(&field).map_err(StorageError::InvalidSchema)?,
                left,
                right,
            ))
        })
        .collect()
}

fn load_aliases(connection: &Connection) -> StorageResult<HashMap<(VocabularyField, String), i64>> {
    let mut statement =
        connection.prepare("SELECT field_name, alias, entity_id FROM vocabulary_aliases")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(field, alias, entity_id)| {
            Ok((
                (
                    VocabularyField::parse(&field).map_err(StorageError::InvalidSchema)?,
                    alias,
                ),
                entity_id,
            ))
        })
        .collect()
}

fn candidate_components<'a>(
    field: VocabularyField,
    values: impl Iterator<Item = &'a String>,
    exclusions: &HashSet<(VocabularyField, String, String)>,
) -> Vec<Vec<String>> {
    let values = values.cloned().collect::<Vec<_>>();
    let mut visited = HashSet::new();
    let mut components = Vec::new();
    for start in &values {
        if !visited.insert(start.clone()) {
            continue;
        }
        let mut component = vec![start.clone()];
        let mut cursor = 0;
        while cursor < component.len() {
            let current = component[cursor].clone();
            for candidate in &values {
                if visited.contains(candidate) {
                    continue;
                }
                let (left, right) = ordered_pair(&current, candidate);
                if !exclusions.contains(&(field, left.to_owned(), right.to_owned())) {
                    visited.insert(candidate.clone());
                    component.push(candidate.clone());
                }
            }
            cursor += 1;
        }
        components.push(component);
    }
    components
}

fn suggested_canonical(
    connection: &Connection,
    field: VocabularyField,
    variants: &[VocabularyVariant],
    mapped_entities: &BTreeSet<i64>,
) -> StorageResult<(String, String)> {
    if mapped_entities.len() == 1 {
        let canonical = connection
            .query_row(
                "SELECT canonical_name FROM canonical_entities WHERE id = ?1 AND status = 'active'",
                [mapped_entities.iter().next().copied().unwrap_or_default()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(canonical) = canonical {
            return Ok((canonical, "沿用已確認的 alias mapping".to_owned()));
        }
    }
    let suggested = variants
        .first()
        .map(|variant| variant.value.clone())
        .unwrap_or_default();
    let reason = format!(
        "{} 中 active collection 使用數最高；未使用模糊比對",
        field.as_str()
    );
    Ok((suggested, reason))
}

fn vocabulary_merge_preflight(
    connection: &Connection,
    field: VocabularyField,
    canonical: &str,
    variants: &[String],
) -> StorageResult<VocabularyMergePreflight> {
    let canonical = canonical.trim();
    let variants = validated_values(variants)?;
    if canonical.is_empty() || variants.len() < 2 {
        return Err(StorageError::InvalidCanonicalMapping(
            "名稱合併必須明確指定 canonical 與至少兩個變體".to_owned(),
        ));
    }
    let selected = selected_values(connection, field)?;
    let variant_set = variants.iter().collect::<HashSet<_>>();
    let matched = selected
        .iter()
        .filter(|row| variant_set.contains(&row.value))
        .collect::<Vec<_>>();
    let affected_collections = matched
        .iter()
        .map(|row| row.collection_id)
        .collect::<BTreeSet<_>>();
    let mut sources: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for row in &matched {
        sources
            .entry(row.source.clone())
            .or_default()
            .insert(row.collection_id);
    }
    let manual_selected_conflicts = matched
        .iter()
        .filter(|row| row.selected_by == "manual" && row.value != canonical)
        .map(|row| row.collection_id)
        .collect::<BTreeSet<_>>()
        .len() as i64;
    let manual_assertions = count_manual_assertions(connection, field, &variant_set)?;
    let saved_views = saved_view_impacts(connection, field, &variant_set, canonical)?;
    Ok(VocabularyMergePreflight {
        field,
        canonical: canonical.to_owned(),
        variants,
        affected_collections: affected_collections.len() as i64,
        source_counts: sources
            .into_iter()
            .map(|(source, ids)| VocabularySourceCount {
                source,
                count: ids.len() as i64,
            })
            .collect(),
        manual_assertions,
        manual_selected_conflicts,
        saved_views,
    })
}

fn count_manual_assertions(
    connection: &Connection,
    field: VocabularyField,
    variants: &HashSet<&String>,
) -> StorageResult<i64> {
    let mut statement = connection.prepare(
        "SELECT assertion.value_json FROM metadata_assertions AS assertion
         JOIN collections AS collection ON collection.id = assertion.collection_id
         WHERE collection.status = 'active' AND assertion.field_name = ?1
           AND assertion.source_kind = 'manual' AND assertion.status IN ('candidate', 'accepted')",
    )?;
    let values = statement
        .query_map([field.assertion_field()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut count = 0;
    for value_json in values {
        if values_from_json(field, &value_json)?
            .iter()
            .any(|(_, value)| variants.contains(value))
        {
            count += 1;
        }
    }
    Ok(count)
}

fn saved_view_impacts(
    connection: &Connection,
    field: VocabularyField,
    variants: &HashSet<&String>,
    canonical: &str,
) -> StorageResult<Vec<VocabularySavedViewImpact>> {
    let mut statement =
        connection.prepare("SELECT id, name, query_json FROM saved_views ORDER BY id")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let key = field.as_str();
    let mut impacts = Vec::new();
    for (id, name, query_json) in rows {
        let query: Value = serde_json::from_str(&query_json)?;
        if let Some(previous) = query.get(key).and_then(Value::as_str)
            && previous != canonical
            && variants.iter().any(|value| value.as_str() == previous)
        {
            impacts.push(VocabularySavedViewImpact {
                id,
                name,
                previous_value: previous.to_owned(),
            });
        }
    }
    Ok(impacts)
}

fn rewrite_saved_views(
    transaction: &Transaction<'_>,
    field: VocabularyField,
    canonical: &str,
    variants: &HashSet<&String>,
) -> StorageResult<i64> {
    let impacts = saved_view_impacts(transaction, field, variants, canonical)?;
    for impact in &impacts {
        let query_json: String = transaction.query_row(
            "SELECT query_json FROM saved_views WHERE id = ?1",
            [impact.id],
            |row| row.get(0),
        )?;
        let mut query: Value = serde_json::from_str(&query_json)?;
        query[field.as_str()] = Value::String(canonical.to_owned());
        transaction.execute(
            "UPDATE saved_views SET query_json = ?2, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![impact.id, serde_json::to_string(&query)?],
        )?;
    }
    Ok(impacts.len() as i64)
}

fn ensure_vocabulary_entity(
    transaction: &Transaction<'_>,
    field: VocabularyField,
    canonical: &str,
) -> StorageResult<i64> {
    let aliased = transaction
        .query_row(
            "SELECT entity_id FROM vocabulary_aliases WHERE field_name = ?1 AND alias = ?2",
            params![field.as_str(), canonical],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if let Some(entity_id) = aliased {
        return Ok(entity_id);
    }
    let existing = transaction.query_row(
        "SELECT id FROM canonical_entities WHERE entity_kind = ?1 AND canonical_name = ?2 AND status = 'active'",
        params![field.entity_kind().as_str(), canonical], |row| row.get::<_, i64>(0),
    ).optional()?;
    if let Some(entity_id) = existing {
        return Ok(entity_id);
    }
    transaction.execute(
        "INSERT INTO canonical_entities(entity_kind, canonical_name, is_official) VALUES (?1, ?2, 0)",
        params![field.entity_kind().as_str(), canonical],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn validated_values(values: &[String]) -> StorageResult<Vec<String>> {
    let mut unique = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(StorageError::InvalidCanonicalMapping(
                "名稱變體不得為空白".to_owned(),
            ));
        }
        unique.insert(value.clone());
    }
    Ok(unique.into_iter().collect())
}

fn ordered_pair<'a>(first: &'a str, second: &'a str) -> (&'a str, &'a str) {
    if first < second {
        (first, second)
    } else {
        (second, first)
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_vocabulary_name;

    #[test]
    fn normalization_is_safe_and_covers_width_spacing_punctuation_and_kana() {
        assert_eq!(
            "alice works",
            normalize_vocabulary_name(" ＡＬＩＣＥ・  Works ")
        );
        assert_eq!("とうほう", normalize_vocabulary_name("トウホウ"));
        assert_ne!(
            normalize_vocabulary_name("Alice"),
            normalize_vocabulary_name("Alicia")
        );
    }
}
