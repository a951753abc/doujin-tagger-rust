//! Library-root configuration snapshots and state changes.

use std::path::{Path, PathBuf};

use doujin_scanner::SourceKind;
use rusqlite::{OptionalExtension, params};

use crate::{
    CatalogRepository, StorageError, StorageResult, path_key, path_text, source_kind_text,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRootSnapshot {
    pub id: i64,
    pub path: PathBuf,
    pub source: SourceKind,
    pub label: String,
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl CatalogRepository {
    pub fn library_roots(&self) -> StorageResult<Vec<LibraryRootSnapshot>> {
        let mut statement = self.connection.prepare(
            "SELECT id, path, source_kind, label, active, created_at, updated_at
             FROM library_roots ORDER BY id",
        )?;
        statement
            .query_map([], map_library_root)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(decode_library_root)
            .collect()
    }

    pub fn library_root(&self, root_id: i64) -> StorageResult<LibraryRootSnapshot> {
        let row = self
            .connection
            .query_row(
                "SELECT id, path, source_kind, label, active, created_at, updated_at
                 FROM library_roots WHERE id = ?1",
                [root_id],
                map_library_root,
            )
            .optional()?
            .ok_or(StorageError::LibraryRootNotFound(root_id))?;
        decode_library_root(row)
    }

    pub fn active_collection_ids_for_roots(&self, root_ids: &[i64]) -> StorageResult<Vec<i64>> {
        if root_ids.is_empty() {
            return Ok(Vec::new());
        }
        let root_ids_json = serde_json::to_string(root_ids)?;
        let mut statement = self.connection.prepare(
            "SELECT collection.id
             FROM collections AS collection
             JOIN collection_locations AS location
               ON location.collection_id = collection.id
              AND location.location_status = 'current'
             JOIN library_roots AS root ON root.id = location.root_id
             JOIN json_each(?1) AS selected_root
               ON CAST(selected_root.value AS INTEGER) = root.id
             WHERE collection.status = 'active' AND root.active = 1
             ORDER BY collection.id",
        )?;
        Ok(statement
            .query_map([root_ids_json], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn deactivate_library_root(&mut self, root_id: i64) -> StorageResult<LibraryRootSnapshot> {
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE library_roots
             SET active = 0,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [root_id],
        )?;
        if changed == 0 {
            return Err(StorageError::LibraryRootNotFound(root_id));
        }
        transaction.commit()?;
        self.library_root(root_id)
    }

    pub fn update_library_root(
        &mut self,
        root_id: i64,
        path: &Path,
        source: SourceKind,
        label: &str,
    ) -> StorageResult<LibraryRootSnapshot> {
        self.library_root(root_id)?;
        validate_library_root(path, label)?;
        let root_path_key = path_key(path);
        let conflicting_root_id = self
            .connection
            .query_row(
                "SELECT id FROM library_roots WHERE path_key = ?1 AND id <> ?2",
                params![root_path_key, root_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(conflicting_root_id) = conflicting_root_id {
            return Err(StorageError::InvalidLibraryRoot(format!(
                "此路徑已由資料夾來源 {conflicting_root_id} 使用：{}",
                path.display()
            )));
        }
        self.connection.execute(
            "UPDATE library_roots
             SET path = ?1, path_key = ?2, source_kind = ?3, label = ?4,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?5",
            params![
                path_text(path)?,
                root_path_key,
                source_kind_text(source),
                label.trim(),
                root_id
            ],
        )?;
        self.library_root(root_id)
    }

    pub fn reactivate_library_root(&mut self, root_id: i64) -> StorageResult<LibraryRootSnapshot> {
        let root = self.library_root(root_id)?;
        validate_library_root(&root.path, &root.label)?;
        self.connection.execute(
            "UPDATE library_roots
             SET active = 1,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [root_id],
        )?;
        self.library_root(root_id)
    }
}

pub(crate) fn validate_library_root(path: &Path, label: &str) -> StorageResult<()> {
    if !path.is_absolute() {
        return Err(StorageError::InvalidLibraryRoot(format!(
            "路徑必須是絕對路徑：{}",
            path.display()
        )));
    }
    if !path.is_dir() {
        return Err(StorageError::InvalidLibraryRoot(format!(
            "library root 不存在或不是資料夾：{}",
            path.display()
        )));
    }
    if label.trim().is_empty() {
        return Err(StorageError::InvalidLibraryRoot(
            "library root label 不得為空白".to_owned(),
        ));
    }
    Ok(())
}

type LibraryRootRow = (i64, String, String, String, bool, String, String);

fn map_library_root(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryRootRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn decode_library_root(row: LibraryRootRow) -> StorageResult<LibraryRootSnapshot> {
    let source = match row.2.as_str() {
        "archive" => SourceKind::Archive,
        "downloads" => SourceKind::Downloads,
        value => {
            return Err(StorageError::InvalidSchema(format!(
                "未知 library root source：{value}"
            )));
        }
    };
    Ok(LibraryRootSnapshot {
        id: row.0,
        path: PathBuf::from(row.1),
        source,
        label: row.3,
        active: row.4,
        created_at: row.5,
        updated_at: row.6,
    })
}
