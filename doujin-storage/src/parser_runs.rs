//! Read access to persisted filename-parser evidence.

use doujin_parser::domain::{Identifier, ParseResult};
use rusqlite::OptionalExtension;

use crate::{CatalogRepository, StorageResult};

impl CatalogRepository {
    pub fn latest_parser_identifiers(&self, collection_id: i64) -> StorageResult<Vec<Identifier>> {
        self.collection_status(collection_id)?;
        let result_json = self
            .connection
            .query_row(
                "SELECT result_json FROM parser_runs
                 WHERE collection_id = ?1 ORDER BY id DESC LIMIT 1",
                [collection_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(result_json) = result_json else {
            return Ok(Vec::new());
        };
        let result: ParseResult = serde_json::from_str(&result_json)?;
        Ok(result.identifiers)
    }
}
