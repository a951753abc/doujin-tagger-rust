//! Persistent, explicit collection lists for cross-query work.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use crate::collections::CollectionSnapshot;
use crate::{CatalogRepository, StorageError, StorageResult};

pub const DEFAULT_WORK_BASKET_ID: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkBasketSummary {
    pub id: i64,
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkBasketItemSnapshot {
    pub collection: CollectionSnapshot,
    pub added_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkBasketSnapshot {
    pub id: i64,
    pub name: String,
    pub items: Vec<WorkBasketItemSnapshot>,
}

impl CatalogRepository {
    pub fn work_baskets(&self) -> StorageResult<Vec<WorkBasketSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT basket.id, basket.name, count(collection.id)
             FROM work_baskets AS basket
             LEFT JOIN work_basket_items AS item ON item.basket_id = basket.id
             LEFT JOIN collections AS collection
               ON collection.id = item.collection_id AND collection.status = 'active'
             GROUP BY basket.id, basket.name
             ORDER BY basket.id",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(WorkBasketSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    count: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn work_basket(&self, basket_id: i64) -> StorageResult<WorkBasketSnapshot> {
        let name = work_basket_name(&self.connection, basket_id)?;
        let rows = {
            let mut statement = self.connection.prepare(
                "SELECT item.collection_id, item.added_at
                 FROM work_basket_items AS item
                 JOIN collections AS collection ON collection.id = item.collection_id
                 WHERE item.basket_id = ?1 AND collection.status = 'active'
                 ORDER BY item.added_at, item.collection_id",
            )?;
            statement
                .query_map([basket_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let items = rows
            .into_iter()
            .map(|(collection_id, added_at)| {
                Ok(WorkBasketItemSnapshot {
                    collection: self.collection(collection_id)?,
                    added_at,
                })
            })
            .collect::<StorageResult<_>>()?;
        Ok(WorkBasketSnapshot {
            id: basket_id,
            name,
            items,
        })
    }

    pub fn add_to_work_basket(
        &mut self,
        basket_id: i64,
        collection_ids: &[i64],
    ) -> StorageResult<WorkBasketSnapshot> {
        let transaction = self.connection.transaction()?;
        work_basket_name(&transaction, basket_id)?;
        let mut unique = HashSet::with_capacity(collection_ids.len());
        for &collection_id in collection_ids {
            if !unique.insert(collection_id) {
                continue;
            }
            let active: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM collections WHERE id = ?1 AND status = 'active'
                 )",
                [collection_id],
                |row| row.get(0),
            )?;
            if !active {
                return Err(StorageError::CollectionNotFound(collection_id));
            }
            transaction.execute(
                "INSERT INTO work_basket_items(basket_id, collection_id)
                 VALUES (?1, ?2)
                 ON CONFLICT(basket_id, collection_id) DO NOTHING",
                params![basket_id, collection_id],
            )?;
        }
        transaction.commit()?;
        self.work_basket(basket_id)
    }

    pub fn remove_from_work_basket(
        &mut self,
        basket_id: i64,
        collection_id: i64,
    ) -> StorageResult<bool> {
        let transaction = self.connection.transaction()?;
        work_basket_name(&transaction, basket_id)?;
        let changed = transaction.execute(
            "DELETE FROM work_basket_items
             WHERE basket_id = ?1 AND collection_id = ?2",
            params![basket_id, collection_id],
        )? > 0;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn clear_work_basket(&mut self, basket_id: i64) -> StorageResult<usize> {
        let transaction = self.connection.transaction()?;
        work_basket_name(&transaction, basket_id)?;
        let changed = transaction.execute(
            "DELETE FROM work_basket_items WHERE basket_id = ?1",
            [basket_id],
        )?;
        transaction.commit()?;
        Ok(changed)
    }
}

fn work_basket_name(connection: &Connection, basket_id: i64) -> StorageResult<String> {
    connection
        .query_row(
            "SELECT name FROM work_baskets WHERE id = ?1",
            [basket_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StorageError::WorkBasketNotFound(basket_id))
}

pub(crate) fn transfer_work_basket_memberships(
    connection: &Connection,
    survivor_collection_id: i64,
    merged_collection_id: i64,
) -> StorageResult<()> {
    connection.execute(
        "INSERT INTO work_basket_items(basket_id, collection_id, added_at)
         SELECT basket_id, ?1, added_at
         FROM work_basket_items
         WHERE collection_id = ?2
         ON CONFLICT(basket_id, collection_id) DO NOTHING",
        params![survivor_collection_id, merged_collection_id],
    )?;
    connection.execute(
        "DELETE FROM work_basket_items WHERE collection_id = ?1",
        [merged_collection_id],
    )?;
    Ok(())
}
