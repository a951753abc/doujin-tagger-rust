//! Aggregate read model for the active collection library.

use rusqlite::{Row, params};

use crate::{CatalogRepository, StorageResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedCount {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionFacet {
    Event,
    Circle,
    Author,
    Parody,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionStatistics {
    pub total: i64,
    pub tagged: i64,
    pub missing_metadata: i64,
    pub classifications: Vec<NamedCount>,
    pub top_parodies: Vec<NamedCount>,
    pub top_authors: Vec<NamedCount>,
    pub top_circles: Vec<NamedCount>,
    pub top_events: Vec<NamedCount>,
    pub top_tags: Vec<NamedCount>,
}

impl CatalogRepository {
    pub fn collection_statistics(&self) -> StorageResult<CollectionStatistics> {
        let total = self.connection.query_row(
            "SELECT count(*) FROM collections AS collection
             WHERE collection.status = 'active'
               AND EXISTS (
                   SELECT 1 FROM collection_locations AS location
                   WHERE location.collection_id = collection.id
                     AND location.location_status = 'current'
               )",
            [],
            |row| row.get(0),
        )?;
        let tagged = self.connection.query_row(
            "SELECT count(DISTINCT tag.collection_id)
             FROM collection_tags AS tag
             JOIN collections AS collection ON collection.id = tag.collection_id
             WHERE collection.status = 'active'
               AND EXISTS (
                   SELECT 1 FROM collection_locations AS location
                   WHERE location.collection_id = collection.id
                     AND location.location_status = 'current'
               )",
            [],
            |row| row.get(0),
        )?;
        let missing_metadata = self.connection.query_row(
            "SELECT count(*)
             FROM collections AS collection
             JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
             WHERE collection.status = 'active'
               AND EXISTS (
                   SELECT 1 FROM collection_locations AS location
                   WHERE location.collection_id = collection.id
                     AND location.location_status = 'current'
               )
               AND (
                   metadata.title IS NULL OR trim(metadata.title) = ''
                   OR metadata.event IS NULL OR trim(metadata.event) = ''
                   OR metadata.circle IS NULL OR trim(metadata.circle) = ''
                   OR json_array_length(metadata.authors_json) = 0
                   OR metadata.parody IS NULL OR trim(metadata.parody) = ''
                   OR metadata.classification_top IS NULL
                   OR trim(metadata.classification_top) = ''
               )",
            [],
            |row| row.get(0),
        )?;
        Ok(CollectionStatistics {
            total,
            tagged,
            missing_metadata,
            classifications: named_counts(
                &self.connection,
                "SELECT COALESCE(NULLIF(trim(metadata.classification_top), ''), '未分類') AS name,
                        count(*) AS item_count
                 FROM collections AS collection
                 JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
                 WHERE collection.status = 'active'
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                 GROUP BY name ORDER BY item_count DESC, name COLLATE NOCASE",
            )?,
            top_parodies: named_counts(
                &self.connection,
                "SELECT metadata.parody AS name, count(*) AS item_count
                 FROM collections AS collection
                 JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
                 WHERE collection.status = 'active' AND trim(COALESCE(metadata.parody, '')) <> ''
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                 GROUP BY metadata.parody
                 ORDER BY item_count DESC, name COLLATE NOCASE LIMIT 20",
            )?,
            top_authors: named_counts(
                &self.connection,
                "SELECT author.value AS name, count(*) AS item_count
                 FROM collections AS collection
                 JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
                 JOIN json_each(metadata.authors_json) AS author
                 WHERE collection.status = 'active'
                   AND author.type = 'text' AND trim(author.value) <> ''
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                 GROUP BY author.value
                 ORDER BY item_count DESC, name COLLATE NOCASE LIMIT 20",
            )?,
            top_circles: named_counts(
                &self.connection,
                "SELECT metadata.circle AS name, count(*) AS item_count
                 FROM collections AS collection
                 JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
                 WHERE collection.status = 'active' AND trim(COALESCE(metadata.circle, '')) <> ''
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                 GROUP BY metadata.circle
                 ORDER BY item_count DESC, name COLLATE NOCASE LIMIT 20",
            )?,
            top_events: named_counts(
                &self.connection,
                "SELECT metadata.event AS name, count(*) AS item_count
                 FROM collections AS collection
                 JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
                 WHERE collection.status = 'active' AND trim(COALESCE(metadata.event, '')) <> ''
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                GROUP BY metadata.event
                 ORDER BY item_count DESC, name COLLATE NOCASE LIMIT 30",
            )?,
            top_tags: named_counts(
                &self.connection,
                "SELECT tag.name AS name, count(*) AS item_count
                 FROM collection_tags AS collection_tag
                 JOIN tags AS tag ON tag.id = collection_tag.tag_id
                 JOIN collections AS collection ON collection.id = collection_tag.collection_id
                 WHERE collection.status = 'active'
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                 GROUP BY tag.name
                 ORDER BY item_count DESC, name COLLATE NOCASE LIMIT 20",
            )?,
        })
    }

    pub fn collection_facets(
        &self,
        facet: CollectionFacet,
        search: &str,
        limit: u32,
    ) -> StorageResult<Vec<NamedCount>> {
        let sql = match facet {
            CollectionFacet::Event => {
                "SELECT metadata.event AS name, count(DISTINCT collection.id) AS item_count
                 FROM collections AS collection
                 JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
                 WHERE collection.status = 'active'
                   AND trim(COALESCE(metadata.event, '')) <> ''
                   AND (?1 = '' OR metadata.event COLLATE NOCASE LIKE ?2 ESCAPE '\\')
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                 GROUP BY metadata.event
                 ORDER BY item_count DESC, name COLLATE NOCASE
                 LIMIT ?3"
            }
            CollectionFacet::Circle => {
                "SELECT metadata.circle AS name, count(DISTINCT collection.id) AS item_count
                 FROM collections AS collection
                 JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
                 WHERE collection.status = 'active'
                   AND trim(COALESCE(metadata.circle, '')) <> ''
                   AND (?1 = '' OR metadata.circle COLLATE NOCASE LIKE ?2 ESCAPE '\\')
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                 GROUP BY metadata.circle
                 ORDER BY item_count DESC, name COLLATE NOCASE
                 LIMIT ?3"
            }
            CollectionFacet::Author => {
                "SELECT author.value AS name, count(DISTINCT collection.id) AS item_count
                 FROM collections AS collection
                 JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
                 JOIN json_each(metadata.authors_json) AS author
                 WHERE collection.status = 'active'
                   AND author.type = 'text' AND trim(author.value) <> ''
                   AND (?1 = '' OR author.value COLLATE NOCASE LIKE ?2 ESCAPE '\\')
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                 GROUP BY author.value
                 ORDER BY item_count DESC, name COLLATE NOCASE
                 LIMIT ?3"
            }
            CollectionFacet::Parody => {
                "SELECT metadata.parody AS name, count(DISTINCT collection.id) AS item_count
                 FROM collections AS collection
                 JOIN effective_metadata AS metadata ON metadata.collection_id = collection.id
                 WHERE collection.status = 'active'
                   AND trim(COALESCE(metadata.parody, '')) <> ''
                   AND (?1 = '' OR metadata.parody COLLATE NOCASE LIKE ?2 ESCAPE '\\')
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                 GROUP BY metadata.parody
                 ORDER BY item_count DESC, name COLLATE NOCASE
                 LIMIT ?3"
            }
            CollectionFacet::Tag => {
                "SELECT tag.name AS name, count(DISTINCT collection.id) AS item_count
                 FROM collection_tags AS collection_tag
                 JOIN tags AS tag ON tag.id = collection_tag.tag_id
                 JOIN collections AS collection ON collection.id = collection_tag.collection_id
                 WHERE collection.status = 'active'
                   AND (?1 = '' OR tag.name COLLATE NOCASE LIKE ?2 ESCAPE '\\')
                   AND EXISTS (
                       SELECT 1 FROM collection_locations AS location
                       WHERE location.collection_id = collection.id
                         AND location.location_status = 'current'
                   )
                 GROUP BY tag.name
                 ORDER BY item_count DESC, name COLLATE NOCASE
                 LIMIT ?3"
            }
        };
        let search = search.trim();
        let pattern = format!("%{}%", escape_like(search));
        let limit = i64::from(limit.clamp(1, 50));
        let mut statement = self.connection.prepare(sql)?;
        Ok(statement
            .query_map(params![search, pattern, limit], map_named_count)?
            .collect::<Result<Vec<_>, _>>()?)
    }
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn named_counts(connection: &rusqlite::Connection, sql: &str) -> StorageResult<Vec<NamedCount>> {
    let mut statement = connection.prepare(sql)?;
    Ok(statement
        .query_map([], map_named_count)?
        .collect::<Result<Vec<_>, _>>()?)
}

fn map_named_count(row: &Row<'_>) -> rusqlite::Result<NamedCount> {
    Ok(NamedCount {
        name: row.get(0)?,
        count: row.get(1)?,
    })
}
