//! Persisted, allowlisted Library query rules.

use doujin_scanner::SourceKind;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::collections::{
    CollectionFilters, CollectionQuery, CollectionSort, MissingMetadataField, SortDirection,
};
use crate::{CatalogRepository, StorageError, StorageResult};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SavedViewLayout {
    List,
    #[default]
    Grid,
}

impl SavedViewLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Grid => "grid",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "list" => Ok(Self::List),
            "grid" => Ok(Self::Grid),
            _ => Err(StorageError::InvalidSavedView(
                "layout 必須是 list 或 grid".to_owned(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedViewQuery {
    pub search: Option<String>,
    pub sort: CollectionSort,
    pub direction: SortDirection,
    pub filters: CollectionFilters,
    pub layout: SavedViewLayout,
}

impl SavedViewQuery {
    pub fn from_collection_query(query: &CollectionQuery, layout: SavedViewLayout) -> Self {
        Self {
            search: query.search.clone(),
            sort: query.sort,
            direction: query.direction,
            filters: query.filters.clone(),
            layout,
        }
    }

    pub fn collection_query(&self) -> CollectionQuery {
        CollectionQuery {
            search: self.search.clone(),
            page: 1,
            per_page: 1,
            sort: self.sort,
            direction: self.direction,
            filters: self.filters.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedViewSnapshot {
    pub id: i64,
    pub name: String,
    pub query: SavedViewQuery,
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedSavedViewQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    q: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    classification: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    missing: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    circle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parody: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subcategory: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tag: Vec<String>,
    #[serde(default)]
    untagged: bool,
    sort: String,
    direction: String,
}

impl CatalogRepository {
    pub fn saved_views(&self) -> StorageResult<Vec<SavedViewSnapshot>> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, query_json, layout, pinned, created_at, updated_at
             FROM saved_views
             ORDER BY pinned DESC, updated_at DESC, id DESC",
        )?;
        statement
            .query_map([], map_saved_view)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(decode_saved_view)
            .collect()
    }

    pub fn saved_view(&self, saved_view_id: i64) -> StorageResult<SavedViewSnapshot> {
        let row = self
            .connection
            .query_row(
                "SELECT id, name, query_json, layout, pinned, created_at, updated_at
                 FROM saved_views WHERE id = ?1",
                [saved_view_id],
                map_saved_view,
            )
            .optional()?
            .ok_or(StorageError::SavedViewNotFound(saved_view_id))?;
        decode_saved_view(row)
    }

    pub fn create_saved_view(
        &mut self,
        name: &str,
        query: &SavedViewQuery,
        pinned: bool,
    ) -> StorageResult<SavedViewSnapshot> {
        let name = validate_name(name)?;
        ensure_unique_name(&self.connection, name, None)?;
        let query_json = encode_query(query)?;
        self.connection.execute(
            "INSERT INTO saved_views(name, query_json, layout, pinned)
             VALUES (?1, ?2, ?3, ?4)",
            params![name, query_json, query.layout.as_str(), pinned],
        )?;
        self.saved_view(self.connection.last_insert_rowid())
    }

    pub fn update_saved_view(
        &mut self,
        saved_view_id: i64,
        name: &str,
        query: &SavedViewQuery,
        pinned: bool,
    ) -> StorageResult<SavedViewSnapshot> {
        self.saved_view(saved_view_id)?;
        let name = validate_name(name)?;
        ensure_unique_name(&self.connection, name, Some(saved_view_id))?;
        let query_json = encode_query(query)?;
        self.connection.execute(
            "UPDATE saved_views
             SET name = ?2, query_json = ?3, layout = ?4, pinned = ?5,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![
                saved_view_id,
                name,
                query_json,
                query.layout.as_str(),
                pinned
            ],
        )?;
        self.saved_view(saved_view_id)
    }

    pub fn delete_saved_view(&mut self, saved_view_id: i64) -> StorageResult<()> {
        if self
            .connection
            .execute("DELETE FROM saved_views WHERE id = ?1", [saved_view_id])?
            == 0
        {
            return Err(StorageError::SavedViewNotFound(saved_view_id));
        }
        Ok(())
    }
}

struct RawSavedView {
    id: i64,
    name: String,
    query_json: String,
    layout: String,
    pinned: bool,
    created_at: String,
    updated_at: String,
}

fn map_saved_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawSavedView> {
    Ok(RawSavedView {
        id: row.get(0)?,
        name: row.get(1)?,
        query_json: row.get(2)?,
        layout: row.get(3)?,
        pinned: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn decode_saved_view(row: RawSavedView) -> StorageResult<SavedViewSnapshot> {
    let mut query = decode_query(&row.query_json)?;
    query.layout = SavedViewLayout::parse(&row.layout)?;
    Ok(SavedViewSnapshot {
        id: row.id,
        name: row.name,
        query,
        pinned: row.pinned,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn validate_name(name: &str) -> StorageResult<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(StorageError::InvalidSavedView(
            "名稱必須是 1 到 80 個字元".to_owned(),
        ));
    }
    Ok(name)
}

fn ensure_unique_name(
    connection: &rusqlite::Connection,
    name: &str,
    except_id: Option<i64>,
) -> StorageResult<()> {
    let conflict = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM saved_views
             WHERE name = ?1 COLLATE NOCASE AND (?2 IS NULL OR id != ?2)
         )",
        params![name, except_id],
        |row| row.get::<_, bool>(0),
    )?;
    if conflict {
        Err(StorageError::SavedViewNameConflict(name.to_owned()))
    } else {
        Ok(())
    }
}

fn encode_query(query: &SavedViewQuery) -> StorageResult<String> {
    serde_json::to_string(&PersistedSavedViewQuery {
        q: query.search.clone(),
        source: query.filters.source.map(|source| match source {
            SourceKind::Archive => "archive".to_owned(),
            SourceKind::Downloads => "downloads".to_owned(),
        }),
        classification: query.filters.classification.clone(),
        missing: query
            .filters
            .missing
            .iter()
            .map(|field| missing_name(*field).to_owned())
            .collect(),
        event: query.filters.event.clone(),
        circle: query.filters.circle.clone(),
        author: query.filters.author.clone(),
        parody: query.filters.parody.clone(),
        subcategory: query.filters.subcategory.clone(),
        tag: query.filters.tags.clone(),
        untagged: query.filters.untagged,
        sort: sort_name(query.sort).to_owned(),
        direction: direction_name(query.direction).to_owned(),
    })
    .map_err(Into::into)
}

fn decode_query(value: &str) -> StorageResult<SavedViewQuery> {
    let persisted: PersistedSavedViewQuery = serde_json::from_str(value)?;
    Ok(SavedViewQuery {
        search: persisted.q,
        sort: match persisted.sort.as_str() {
            "created" => CollectionSort::Created,
            "updated" => CollectionSort::Updated,
            "title" => CollectionSort::Title,
            _ => return invalid_saved_query("sort"),
        },
        direction: match persisted.direction.as_str() {
            "asc" => SortDirection::Ascending,
            "desc" => SortDirection::Descending,
            _ => return invalid_saved_query("direction"),
        },
        filters: CollectionFilters {
            event: persisted.event,
            circle: persisted.circle,
            author: persisted.author,
            parody: persisted.parody,
            classification: persisted.classification,
            subcategory: persisted.subcategory,
            source: match persisted.source.as_deref() {
                None => None,
                Some("archive") => Some(SourceKind::Archive),
                Some("downloads") => Some(SourceKind::Downloads),
                Some(_) => return invalid_saved_query("source"),
            },
            tags: persisted.tag,
            untagged: persisted.untagged,
            missing: persisted
                .missing
                .iter()
                .map(|value| parse_missing(value))
                .collect::<StorageResult<_>>()?,
        },
        layout: SavedViewLayout::default(),
    })
}

fn sort_name(sort: CollectionSort) -> &'static str {
    match sort {
        CollectionSort::Created => "created",
        CollectionSort::Updated => "updated",
        CollectionSort::Title => "title",
    }
}

fn direction_name(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Ascending => "asc",
        SortDirection::Descending => "desc",
    }
}

fn missing_name(field: MissingMetadataField) -> &'static str {
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

fn parse_missing(value: &str) -> StorageResult<MissingMetadataField> {
    match value {
        "any" => Ok(MissingMetadataField::Any),
        "title" => Ok(MissingMetadataField::Title),
        "event" => Ok(MissingMetadataField::Event),
        "circle" => Ok(MissingMetadataField::Circle),
        "authors" => Ok(MissingMetadataField::Authors),
        "parody" => Ok(MissingMetadataField::Parody),
        "classification" => Ok(MissingMetadataField::Classification),
        _ => invalid_saved_query("missing"),
    }
}

fn invalid_saved_query<T>(field: &str) -> StorageResult<T> {
    Err(StorageError::InvalidSavedView(format!(
        "保存的 query 包含不支援的 {field} 值"
    )))
}
