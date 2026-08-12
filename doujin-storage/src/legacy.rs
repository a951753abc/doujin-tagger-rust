//! Transactional import boundary for the read-only legacy catalog migration.

use std::collections::HashMap;
use std::path::PathBuf;

use doujin_parser::domain::{Authors, Classification, Parody};
use doujin_scanner::SourceKind;
use rusqlite::{Transaction, params};

use crate::metadata::{MetadataField, MetadataValue};
use crate::{
    AssertionInsert, CatalogRepository, StorageError, StorageResult, insert_assertion, path_key,
    rebuild_projection, select_assertion, upsert_library_root,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyRootSource {
    Archive,
    Downloads,
}

impl LegacyRootSource {
    fn scanner_source(self) -> SourceKind {
        match self {
            Self::Archive => SourceKind::Archive,
            Self::Downloads => SourceKind::Downloads,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyLibraryRoot {
    pub path: PathBuf,
    pub source: LegacyRootSource,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyMediaKind {
    Zip,
    ImageFolder,
}

impl LegacyMediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::ImageFolder => "image_folder",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyCollection {
    pub id: i64,
    pub filename: String,
    pub filepath: PathBuf,
    pub media_kind: LegacyMediaKind,
    pub root_path: PathBuf,
    pub relative_path: String,
    pub event: Option<String>,
    pub circle: Option<String>,
    pub authors: Authors,
    pub title: String,
    pub parody: Option<Parody>,
    pub classification: Classification,
    pub is_dl: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyTag {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyTagLink {
    pub collection_id: i64,
    pub tag_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyCatalog {
    pub roots: Vec<LegacyLibraryRoot>,
    pub collections: Vec<LegacyCollection>,
    pub tags: Vec<LegacyTag>,
    pub tag_links: Vec<LegacyTagLink>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyImportCounts {
    pub roots: usize,
    pub collections: usize,
    pub locations: usize,
    pub assertions: usize,
    pub selections: usize,
    pub effective_metadata: usize,
    pub tags: usize,
    pub tag_links: usize,
}

impl CatalogRepository {
    /// Imports one already validated legacy snapshot in a single transaction.
    /// The repository must be a newly initialized v2 catalog.
    pub fn import_legacy_catalog(
        &mut self,
        catalog: &LegacyCatalog,
    ) -> StorageResult<LegacyImportCounts> {
        let transaction = self.connection.transaction()?;
        ensure_empty_target(&transaction)?;

        let mut root_ids = HashMap::with_capacity(catalog.roots.len());
        for root in &catalog.roots {
            if root.label.trim().is_empty() {
                return Err(StorageError::InvalidLegacyImport(format!(
                    "library root {} 的 label 不得為空白",
                    root.path.display()
                )));
            }
            let root_id = upsert_library_root(
                &transaction,
                &root.path,
                root.source.scanner_source(),
                &root.label,
            )?;
            root_ids.insert(path_key(&root.path), root_id);
        }

        let mut assertion_count = 0_usize;
        for collection in &catalog.collections {
            validate_collection(collection)?;
            let root_key = path_key(&collection.root_path);
            let root_id = root_ids.get(&root_key).copied().ok_or_else(|| {
                StorageError::InvalidLegacyImport(format!(
                    "收藏 {} 指向未匯入的 library root：{}",
                    collection.id,
                    collection.root_path.display()
                ))
            })?;

            transaction.execute(
                "INSERT INTO collections(
                     id, status, media_kind, parser_version, created_at, updated_at
                 ) VALUES (
                     ?1, 'active', ?2, NULL,
                     COALESCE(?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                     COALESCE(?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                 )",
                params![
                    collection.id,
                    collection.media_kind.as_str(),
                    nonempty(collection.created_at.as_deref()),
                    nonempty(collection.updated_at.as_deref()),
                ],
            )?;
            transaction.execute(
                "INSERT INTO collection_locations(
                     collection_id, root_id, full_path, path_key, relative_path, filename,
                     location_status, discovered_at
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, 'current',
                     COALESCE(?7, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                 )",
                params![
                    collection.id,
                    root_id,
                    collection.filepath.to_string_lossy(),
                    path_key(&collection.filepath),
                    collection.relative_path,
                    collection.filename,
                    nonempty(collection.created_at.as_deref()),
                ],
            )?;

            let mut values = vec![
                (
                    MetadataField::Title,
                    MetadataValue::Text(collection.title.clone()),
                ),
                (
                    MetadataField::Classification,
                    MetadataValue::Classification(collection.classification.clone()),
                ),
                (
                    MetadataField::IsDl,
                    MetadataValue::Boolean(collection.is_dl),
                ),
            ];
            if let Some(value) = &collection.event {
                values.push((MetadataField::Event, MetadataValue::Text(value.clone())));
            }
            if let Some(value) = &collection.circle {
                values.push((MetadataField::Circle, MetadataValue::Text(value.clone())));
            }
            if !collection.authors.values.is_empty() {
                values.push((
                    MetadataField::Authors,
                    MetadataValue::Authors(collection.authors.clone()),
                ));
            }
            if let Some(value) = &collection.parody {
                values.push((MetadataField::Parody, MetadataValue::Parody(value.clone())));
            }

            let source_reference = format!("legacy:doujinshi:{}", collection.id);
            for (field, value) in values {
                let value_json = value
                    .into_json_for(field)
                    .map_err(StorageError::InvalidLegacyImport)?;
                let assertion_id = insert_assertion(
                    &transaction,
                    AssertionInsert {
                        collection_id: collection.id,
                        field,
                        value_json,
                        source_kind: "legacy",
                        status: "accepted",
                        parser_run_id: None,
                        source_reference: Some(&source_reference),
                        confidence_total: None,
                        reason: Some("imported_from_legacy_catalog"),
                    },
                )?;
                select_assertion(
                    &transaction,
                    collection.id,
                    field,
                    assertion_id,
                    "migration",
                )?;
                assertion_count += 1;
            }
            rebuild_projection(&transaction, collection.id)?;
        }

        for tag in &catalog.tags {
            if tag.id <= 0 || tag.name.trim().is_empty() {
                return Err(StorageError::InvalidLegacyImport(format!(
                    "tag {} 的 ID 或名稱無效",
                    tag.id
                )));
            }
            transaction.execute(
                "INSERT INTO tags(id, name) VALUES (?1, ?2)",
                params![tag.id, tag.name],
            )?;
        }
        for link in &catalog.tag_links {
            transaction.execute(
                "INSERT INTO collection_tags(collection_id, tag_id) VALUES (?1, ?2)",
                params![link.collection_id, link.tag_id],
            )?;
        }

        transaction.commit()?;
        Ok(LegacyImportCounts {
            roots: catalog.roots.len(),
            collections: catalog.collections.len(),
            locations: catalog.collections.len(),
            assertions: assertion_count,
            selections: assertion_count,
            effective_metadata: catalog.collections.len(),
            tags: catalog.tags.len(),
            tag_links: catalog.tag_links.len(),
        })
    }
}

fn ensure_empty_target(transaction: &Transaction<'_>) -> StorageResult<()> {
    let has_data: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM collections
             UNION ALL SELECT 1 FROM library_roots
             UNION ALL SELECT 1 FROM tags
             UNION ALL SELECT 1 FROM scan_runs
             UNION ALL SELECT 1 FROM background_jobs
             UNION ALL SELECT 1 FROM file_operations
         )",
        [],
        |row| row.get(0),
    )?;
    if has_data {
        return Err(StorageError::LegacyImportRequiresEmptyCatalog);
    }
    Ok(())
}

fn validate_collection(collection: &LegacyCollection) -> StorageResult<()> {
    if collection.id <= 0 {
        return Err(StorageError::InvalidLegacyImport(format!(
            "collection ID {} 無效",
            collection.id
        )));
    }
    for (field, value) in [
        ("filename", collection.filename.as_str()),
        ("filepath", collection.filepath.to_string_lossy().as_ref()),
        ("relative_path", collection.relative_path.as_str()),
        ("title", collection.title.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(StorageError::InvalidLegacyImport(format!(
                "收藏 {} 的 {field} 不得為空白",
                collection.id
            )));
        }
    }
    Ok(())
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}
