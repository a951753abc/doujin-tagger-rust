//! Read models for the active collection library.

use std::path::PathBuf;

use doujin_scanner::SourceKind;
use rusqlite::types::Value as SqlValue;
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::metadata::MetadataHistory;
use crate::{CatalogRepository, StorageError, StorageResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionQuery {
    pub search: Option<String>,
    pub page: u32,
    pub per_page: u32,
    pub sort: CollectionSort,
    pub direction: SortDirection,
    pub filters: CollectionFilters,
}

impl Default for CollectionQuery {
    fn default() -> Self {
        Self {
            search: None,
            page: 1,
            per_page: 50,
            sort: CollectionSort::Created,
            direction: SortDirection::Descending,
            filters: CollectionFilters::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CollectionSort {
    #[default]
    Created,
    Updated,
    Title,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    #[default]
    Descending,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectionFilters {
    pub event: Option<String>,
    pub circle: Option<String>,
    pub author: Option<String>,
    pub parody: Option<String>,
    pub classification: Option<String>,
    pub subcategory: Option<String>,
    pub source: Option<SourceKind>,
    pub tags: Vec<String>,
    pub untagged: bool,
    pub missing: Vec<MissingMetadataField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingMetadataField {
    Any,
    Title,
    Event,
    Circle,
    Authors,
    Parody,
    Classification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionPage {
    pub items: Vec<CollectionSnapshot>,
    pub page: u32,
    pub per_page: u32,
    pub total: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewQueueQuery {
    pub page: u32,
    pub per_page: u32,
    pub kind: ReviewQueueKind,
}

impl Default for ReviewQueueQuery {
    fn default() -> Self {
        Self {
            page: 1,
            per_page: 100,
            kind: ReviewQueueKind::All,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewQueueKind {
    #[default]
    All,
    Missing,
    Candidate,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewQueuePage {
    pub items: Vec<ReviewQueueItem>,
    pub page: u32,
    pub per_page: u32,
    pub total: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewQueueItem {
    pub collection: CollectionSnapshot,
    pub metadata: MetadataHistory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionQueryLocation {
    pub collection: CollectionSnapshot,
    pub position: Option<i64>,
    pub page: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionRootSnapshot {
    pub id: i64,
    pub source: SourceKind,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionSnapshot {
    pub id: i64,
    pub path: PathBuf,
    pub filename: String,
    pub root: Option<CollectionRootSnapshot>,
    pub title: Option<String>,
    pub event: Option<String>,
    pub circle: Option<String>,
    pub authors: Vec<String>,
    pub parody: Option<String>,
    pub parody_raw: Option<String>,
    pub classification_top: Option<String>,
    pub classification_subcategory: Option<String>,
    pub is_dl: Option<bool>,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl CatalogRepository {
    pub fn add_collection_tag(&mut self, collection_id: i64, tag_name: &str) -> StorageResult<i64> {
        let tag_name = tag_name.trim();
        if tag_name.is_empty() {
            return Err(StorageError::InvalidMetadata(
                "tag name 不得為空白".to_owned(),
            ));
        }
        let transaction = self.connection.transaction()?;
        super::ensure_collection(&transaction, collection_id)?;
        transaction.execute(
            "INSERT INTO tags(name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
            [tag_name],
        )?;
        let tag_id =
            transaction.query_row("SELECT id FROM tags WHERE name = ?1", [tag_name], |row| {
                row.get(0)
            })?;
        transaction.execute(
            "INSERT INTO collection_tags(collection_id, tag_id)
             VALUES (?1, ?2) ON CONFLICT(collection_id, tag_id) DO NOTHING",
            params![collection_id, tag_id],
        )?;
        transaction.commit()?;
        Ok(tag_id)
    }

    pub fn remove_collection_tag(
        &mut self,
        collection_id: i64,
        tag_name: &str,
    ) -> StorageResult<bool> {
        let tag_name = tag_name.trim();
        if tag_name.is_empty() {
            return Err(StorageError::InvalidMetadata(
                "tag name 不得為空白".to_owned(),
            ));
        }
        let transaction = self.connection.transaction()?;
        super::ensure_collection(&transaction, collection_id)?;
        let tag_id = transaction
            .query_row("SELECT id FROM tags WHERE name = ?1", [tag_name], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?;
        let Some(tag_id) = tag_id else {
            transaction.commit()?;
            return Ok(false);
        };
        let changed = transaction.execute(
            "DELETE FROM collection_tags WHERE collection_id = ?1 AND tag_id = ?2",
            params![collection_id, tag_id],
        )? > 0;
        if changed {
            transaction.execute(
                "DELETE FROM tags
                 WHERE id = ?1
                   AND NOT EXISTS (SELECT 1 FROM collection_tags WHERE tag_id = ?1)",
                [tag_id],
            )?;
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub fn tag_count(&self) -> StorageResult<i64> {
        Ok(self
            .connection
            .query_row("SELECT count(*) FROM tags", [], |row| row.get(0))?)
    }

    pub fn collections(&self, query: &CollectionQuery) -> StorageResult<CollectionPage> {
        let page = query.page.max(1);
        let per_page = query.per_page.clamp(1, 200);
        let prepared = PreparedConditions::new(query);
        let total_sql = format!(
            "SELECT count(*) {COLLECTION_FROM_SQL} WHERE collection.status = 'active'{}",
            prepared.clause
        );
        let total = self.connection.query_row(
            &total_sql,
            params_from_iter(prepared.parameters.iter()),
            |row| row.get(0),
        )?;

        let offset = i64::from(page - 1) * i64::from(per_page);
        let mut parameters = prepared.parameters;
        parameters.push(SqlValue::Integer(i64::from(per_page)));
        parameters.push(SqlValue::Integer(offset));
        let query_sql = format!(
            "{COLLECTION_SELECT_SQL} {COLLECTION_FROM_SQL}
             WHERE collection.status = 'active'{}
             ORDER BY {} LIMIT ? OFFSET ?",
            prepared.clause,
            collection_order(query)
        );
        let mut statement = self.connection.prepare(&query_sql)?;
        let rows = statement
            .query_map(params_from_iter(parameters.iter()), map_collection_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CollectionPage {
            items: rows
                .into_iter()
                .map(decode_collection_row)
                .collect::<StorageResult<_>>()?,
            page,
            per_page,
            total,
        })
    }

    pub fn review_queue(&self, query: &ReviewQueueQuery) -> StorageResult<ReviewQueuePage> {
        let page = query.page.max(1);
        let per_page = query.per_page.clamp(1, 100);
        let condition = review_queue_condition(query.kind);
        let total_sql = format!(
            "SELECT count(*) {COLLECTION_FROM_SQL}
             WHERE collection.status = 'active' AND ({condition})"
        );
        let total = self
            .connection
            .query_row(&total_sql, [], |row| row.get(0))?;

        let offset = i64::from(page - 1) * i64::from(per_page);
        let query_sql = format!(
            "{COLLECTION_SELECT_SQL} {COLLECTION_FROM_SQL}
             WHERE collection.status = 'active' AND ({condition})
             ORDER BY CASE WHEN {REVIEW_CANDIDATE_CONDITION} THEN 0 ELSE 1 END,
                      metadata.updated_at ASC, collection.id ASC
             LIMIT ?1 OFFSET ?2"
        );
        let collections = {
            let mut statement = self.connection.prepare(&query_sql)?;
            statement
                .query_map(params![i64::from(per_page), offset], map_collection_row)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(decode_collection_row)
                .collect::<StorageResult<Vec<_>>>()?
        };
        let items = collections
            .into_iter()
            .map(|collection| {
                let metadata = self.metadata_history(collection.id)?;
                Ok(ReviewQueueItem {
                    collection,
                    metadata,
                })
            })
            .collect::<StorageResult<Vec<_>>>()?;
        Ok(ReviewQueuePage {
            items,
            page,
            per_page,
            total,
        })
    }

    pub fn collection(&self, collection_id: i64) -> StorageResult<CollectionSnapshot> {
        let sql = format!(
            "{COLLECTION_SELECT_SQL} {COLLECTION_FROM_SQL}
             WHERE collection.status = 'active' AND collection.id = ?1"
        );
        let row = self
            .connection
            .query_row(&sql, [collection_id], map_collection_row)
            .optional()?
            .ok_or(StorageError::CollectionNotFound(collection_id))?;
        decode_collection_row(row)
    }

    pub fn locate_collection(
        &self,
        collection_id: i64,
        query: &CollectionQuery,
    ) -> StorageResult<CollectionQueryLocation> {
        let collection = self.collection(collection_id)?;
        let per_page = query.per_page.clamp(1, 200);
        let prepared = PreparedConditions::new(query);
        let sql = format!(
            "SELECT query_position FROM (
                 SELECT collection.id,
                        row_number() OVER (ORDER BY {}) AS query_position
                 {COLLECTION_FROM_SQL}
                 WHERE collection.status = 'active'{}
             ) AS filtered_collection
             WHERE filtered_collection.id = ?",
            collection_order(query),
            prepared.clause
        );
        let mut parameters = prepared.parameters;
        parameters.push(SqlValue::Integer(collection_id));
        let position = self
            .connection
            .query_row(&sql, params_from_iter(parameters.iter()), |row| row.get(0))
            .optional()?;
        let page = position.map(|position: i64| {
            let zero_based = position.saturating_sub(1);
            ((zero_based / i64::from(per_page)) + 1).min(i64::from(u32::MAX)) as u32
        });
        Ok(CollectionQueryLocation {
            collection,
            position,
            page,
        })
    }
}

const REVIEW_MISSING_CONDITION: &str = "(
    metadata.title IS NULL OR trim(metadata.title) = ''
    OR metadata.event IS NULL OR trim(metadata.event) = ''
    OR metadata.circle IS NULL OR trim(metadata.circle) = ''
    OR json_array_length(metadata.authors_json) = 0
    OR metadata.parody IS NULL OR trim(metadata.parody) = ''
    OR metadata.classification_top IS NULL OR trim(metadata.classification_top) = ''
)";

const REVIEW_CANDIDATE_CONDITION: &str = "EXISTS (
    SELECT 1
    FROM metadata_assertions AS review_assertion
    WHERE review_assertion.collection_id = collection.id
      AND review_assertion.status = 'candidate'
)";

fn review_queue_condition(kind: ReviewQueueKind) -> String {
    match kind {
        ReviewQueueKind::All => {
            format!("{REVIEW_MISSING_CONDITION} OR {REVIEW_CANDIDATE_CONDITION}")
        }
        ReviewQueueKind::Missing => REVIEW_MISSING_CONDITION.to_owned(),
        ReviewQueueKind::Candidate => REVIEW_CANDIDATE_CONDITION.to_owned(),
    }
}

fn collection_order(query: &CollectionQuery) -> &'static str {
    match (query.sort, query.direction) {
        (CollectionSort::Created, SortDirection::Ascending) => {
            "collection.created_at ASC, collection.id ASC"
        }
        (CollectionSort::Created, SortDirection::Descending) => {
            "collection.created_at DESC, collection.id DESC"
        }
        (CollectionSort::Updated, SortDirection::Ascending) => {
            "metadata.updated_at ASC, collection.id ASC"
        }
        (CollectionSort::Updated, SortDirection::Descending) => {
            "metadata.updated_at DESC, collection.id DESC"
        }
        (CollectionSort::Title, SortDirection::Ascending) => {
            "metadata.title IS NULL ASC, metadata.title COLLATE NOCASE ASC, collection.id DESC"
        }
        (CollectionSort::Title, SortDirection::Descending) => {
            "metadata.title IS NULL ASC, metadata.title COLLATE NOCASE DESC, collection.id DESC"
        }
    }
}

const COLLECTION_SELECT_SQL: &str =
    "SELECT collection.id, collection.created_at, metadata.updated_at,
            location.full_path, location.filename,
            root.id, root.source_kind, root.label,
            metadata.title, metadata.event, metadata.circle, metadata.authors_json,
            metadata.parody, metadata.parody_raw,
            metadata.classification_top, metadata.classification_subcategory,
            metadata.is_dl,
            COALESCE((
                SELECT json_group_array(ordered_tags.name)
                FROM (
                    SELECT tag.name
                    FROM collection_tags AS collection_tag
                    JOIN tags AS tag ON tag.id = collection_tag.tag_id
                    WHERE collection_tag.collection_id = collection.id
                    ORDER BY tag.name
                ) AS ordered_tags
            ), '[]')";

const COLLECTION_FROM_SQL: &str = "FROM collections AS collection
     JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
     JOIN collection_locations AS location ON location.id = (
         SELECT candidate_location.id
         FROM collection_locations AS candidate_location
         WHERE candidate_location.collection_id = collection.id
           AND candidate_location.location_status = 'current'
         ORDER BY candidate_location.id DESC LIMIT 1
     )
     LEFT JOIN library_roots AS root ON root.id = location.root_id";

struct PreparedConditions {
    clause: String,
    parameters: Vec<SqlValue>,
}

impl PreparedConditions {
    fn new(query: &CollectionQuery) -> Self {
        let mut prepared = Self {
            clause: String::new(),
            parameters: Vec::new(),
        };
        if let Some(terms) = query
            .search
            .as_deref()
            .map(search_terms)
            .filter(|terms| !terms.is_empty())
        {
            let fts_query = terms
                .iter()
                .map(|term| format!("\"{term}\"*"))
                .collect::<Vec<_>>()
                .join(" ");
            let filename_conditions = std::iter::repeat_n(
                "location.filename COLLATE NOCASE LIKE ? ESCAPE '\\'",
                terms.len(),
            )
            .collect::<Vec<_>>()
            .join(" AND ");
            prepared.clause.push_str(&format!(
                " AND (collection.id IN (
                    SELECT rowid FROM collection_fts WHERE collection_fts MATCH ?
                ) OR ({filename_conditions}))"
            ));
            prepared.parameters.push(SqlValue::Text(fts_query));
            prepared.parameters.extend(
                terms
                    .iter()
                    .map(|term| SqlValue::Text(format!("%{}%", escape_like(term)))),
            );
        }

        prepared.push_exact("metadata.event", query.filters.event.as_deref());
        prepared.push_exact("metadata.circle", query.filters.circle.as_deref());
        if let Some(author) = nonempty(query.filters.author.as_deref()) {
            prepared.clause.push_str(
                " AND EXISTS (
                    SELECT 1 FROM json_each(metadata.authors_json) AS author
                    WHERE author.value = ? COLLATE NOCASE
                )",
            );
            prepared.parameters.push(SqlValue::Text(author.to_owned()));
        }
        prepared.push_exact("metadata.parody", query.filters.parody.as_deref());
        prepared.push_exact(
            "metadata.classification_top",
            query.filters.classification.as_deref(),
        );
        prepared.push_exact(
            "metadata.classification_subcategory",
            query.filters.subcategory.as_deref(),
        );
        if let Some(source) = query.filters.source {
            prepared
                .clause
                .push_str(" AND root.source_kind = ? COLLATE NOCASE");
            prepared.parameters.push(SqlValue::Text(
                match source {
                    SourceKind::Archive => "archive",
                    SourceKind::Downloads => "downloads",
                }
                .to_owned(),
            ));
        }
        for tag in query
            .filters
            .tags
            .iter()
            .filter_map(|tag| nonempty(Some(tag)))
        {
            prepared.clause.push_str(
                " AND EXISTS (
                    SELECT 1
                    FROM collection_tags AS filtered_collection_tag
                    JOIN tags AS filtered_tag ON filtered_tag.id = filtered_collection_tag.tag_id
                    WHERE filtered_collection_tag.collection_id = collection.id
                      AND filtered_tag.name = ? COLLATE NOCASE
                )",
            );
            prepared.parameters.push(SqlValue::Text(tag.to_owned()));
        }
        if query.filters.untagged {
            prepared.clause.push_str(
                " AND NOT EXISTS (
                    SELECT 1 FROM collection_tags AS untagged_collection_tag
                    WHERE untagged_collection_tag.collection_id = collection.id
                )",
            );
        }
        for field in &query.filters.missing {
            prepared.clause.push_str(match field {
                MissingMetadataField::Any => {
                    " AND (
                        metadata.title IS NULL OR trim(metadata.title) = ''
                        OR metadata.event IS NULL OR trim(metadata.event) = ''
                        OR metadata.circle IS NULL OR trim(metadata.circle) = ''
                        OR json_array_length(metadata.authors_json) = 0
                        OR metadata.parody IS NULL OR trim(metadata.parody) = ''
                        OR metadata.classification_top IS NULL
                        OR trim(metadata.classification_top) = ''
                    )"
                }
                MissingMetadataField::Title => {
                    " AND (metadata.title IS NULL OR trim(metadata.title) = '')"
                }
                MissingMetadataField::Event => {
                    " AND (metadata.event IS NULL OR trim(metadata.event) = '')"
                }
                MissingMetadataField::Circle => {
                    " AND (metadata.circle IS NULL OR trim(metadata.circle) = '')"
                }
                MissingMetadataField::Authors => {
                    " AND json_array_length(metadata.authors_json) = 0"
                }
                MissingMetadataField::Parody => {
                    " AND (metadata.parody IS NULL OR trim(metadata.parody) = '')"
                }
                MissingMetadataField::Classification => {
                    " AND (metadata.classification_top IS NULL
                         OR trim(metadata.classification_top) = '')"
                }
            });
        }
        prepared
    }

    fn push_exact(&mut self, column: &str, value: Option<&str>) {
        if let Some(value) = nonempty(value) {
            self.clause
                .push_str(&format!(" AND {column} = ? COLLATE NOCASE"));
            self.parameters.push(SqlValue::Text(value.to_owned()));
        }
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn search_terms(search: &str) -> Vec<String> {
    search
        .replace('"', " ")
        .split_whitespace()
        .map(|term| {
            term.chars()
                .filter(|character| !character.is_control())
                .collect::<String>()
        })
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .collect()
}

fn escape_like(term: &str) -> String {
    term.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

struct RawCollectionRow {
    id: i64,
    created_at: String,
    updated_at: String,
    path: String,
    filename: String,
    root_id: Option<i64>,
    root_source: Option<String>,
    root_label: Option<String>,
    title: Option<String>,
    event: Option<String>,
    circle: Option<String>,
    authors_json: String,
    parody: Option<String>,
    parody_raw: Option<String>,
    classification_top: Option<String>,
    classification_subcategory: Option<String>,
    is_dl: Option<bool>,
    tags_json: String,
}

fn map_collection_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawCollectionRow> {
    Ok(RawCollectionRow {
        id: row.get(0)?,
        created_at: row.get(1)?,
        updated_at: row.get(2)?,
        path: row.get(3)?,
        filename: row.get(4)?,
        root_id: row.get(5)?,
        root_source: row.get(6)?,
        root_label: row.get(7)?,
        title: row.get(8)?,
        event: row.get(9)?,
        circle: row.get(10)?,
        authors_json: row.get(11)?,
        parody: row.get(12)?,
        parody_raw: row.get(13)?,
        classification_top: row.get(14)?,
        classification_subcategory: row.get(15)?,
        is_dl: row.get(16)?,
        tags_json: row.get(17)?,
    })
}

fn decode_collection_row(row: RawCollectionRow) -> StorageResult<CollectionSnapshot> {
    let root = match (row.root_id, row.root_source, row.root_label) {
        (Some(id), Some(source), Some(label)) => Some(CollectionRootSnapshot {
            id,
            source: parse_source(&source)?,
            label,
        }),
        (None, None, None) => None,
        _ => {
            return Err(StorageError::InvalidSchema(format!(
                "收藏 {} 的 library root 資料不完整",
                row.id
            )));
        }
    };
    Ok(CollectionSnapshot {
        id: row.id,
        path: PathBuf::from(row.path),
        filename: row.filename,
        root,
        title: row.title,
        event: row.event,
        circle: row.circle,
        authors: serde_json::from_str(&row.authors_json)?,
        parody: row.parody,
        parody_raw: row.parody_raw,
        classification_top: row.classification_top,
        classification_subcategory: row.classification_subcategory,
        is_dl: row.is_dl,
        tags: serde_json::from_str(&row.tags_json)?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn parse_source(source: &str) -> StorageResult<SourceKind> {
    match source {
        "archive" => Ok(SourceKind::Archive),
        "downloads" => Ok(SourceKind::Downloads),
        value => Err(StorageError::InvalidSchema(format!(
            "未知 library root source：{value}"
        ))),
    }
}
