//! Library-root configuration snapshots and state changes.

use std::path::PathBuf;

use doujin_scanner::SourceKind;
use rusqlite::OptionalExtension;

use crate::{CatalogRepository, StorageError, StorageResult};

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
