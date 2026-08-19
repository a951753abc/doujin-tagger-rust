//! Persisted, allowlisted Shelf homepage composition.

use std::collections::HashSet;

use rusqlite::params;

use crate::{CatalogRepository, StorageError, StorageResult};

pub const DEFAULT_SHELF_PREVIEW_LIMIT: u32 = 8;
pub const SHELF_PREVIEW_LIMIT_CHOICES: [u32; 4] = [6, 8, 12, 16];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShelfType {
    Recent,
    Featured,
    Event,
    SavedView,
}

impl ShelfType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Featured => "featured",
            Self::Event => "event",
            Self::SavedView => "saved_view",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "recent" => Ok(Self::Recent),
            "featured" => Ok(Self::Featured),
            "event" => Ok(Self::Event),
            "saved_view" => Ok(Self::SavedView),
            _ => Err(StorageError::InvalidShelfConfiguration(
                "shelf type 不受支援".to_owned(),
            )),
        }
    }

    fn is_builtin(self) -> bool {
        !matches!(self, Self::SavedView)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfConfigurationItem {
    pub shelf_type: ShelfType,
    pub saved_view_id: Option<i64>,
    pub position: u32,
    pub enabled: bool,
    pub preview_limit: u32,
}

pub fn default_shelf_configuration() -> Vec<ShelfConfigurationItem> {
    [ShelfType::Recent, ShelfType::Featured, ShelfType::Event]
        .into_iter()
        .enumerate()
        .map(|(position, shelf_type)| ShelfConfigurationItem {
            shelf_type,
            saved_view_id: None,
            position: position as u32,
            enabled: true,
            preview_limit: DEFAULT_SHELF_PREVIEW_LIMIT,
        })
        .collect()
}

impl CatalogRepository {
    pub fn shelf_configuration(&self) -> StorageResult<Vec<ShelfConfigurationItem>> {
        let mut statement = self.connection.prepare(
            "SELECT shelf_type, saved_view_id, position, enabled, preview_limit
             FROM shelf_configuration ORDER BY position",
        )?;
        let mut items = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(decode_item)
            .collect::<StorageResult<Vec<_>>>()?;
        if items.is_empty() {
            Ok(default_shelf_configuration())
        } else {
            for (position, item) in items.iter_mut().enumerate() {
                item.position = position as u32;
            }
            Ok(items)
        }
    }

    pub fn replace_shelf_configuration(
        &mut self,
        items: &[ShelfConfigurationItem],
    ) -> StorageResult<Vec<ShelfConfigurationItem>> {
        validate_items(items)?;
        let transaction = self.connection.transaction()?;
        for item in items {
            if let Some(saved_view_id) = item.saved_view_id {
                let exists = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM saved_views WHERE id = ?1)",
                    [saved_view_id],
                    |row| row.get::<_, bool>(0),
                )?;
                if !exists {
                    return Err(StorageError::SavedViewNotFound(saved_view_id));
                }
            }
        }
        transaction.execute("DELETE FROM shelf_configuration", [])?;
        for item in items {
            transaction.execute(
                "INSERT INTO shelf_configuration(position, shelf_type, saved_view_id, enabled, preview_limit)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    i64::from(item.position),
                    item.shelf_type.as_str(),
                    item.saved_view_id,
                    item.enabled,
                    i64::from(item.preview_limit),
                ],
            )?;
        }
        transaction.commit()?;
        self.shelf_configuration()
    }

    pub fn reset_shelf_configuration(&mut self) -> StorageResult<Vec<ShelfConfigurationItem>> {
        let defaults = default_shelf_configuration();
        self.replace_shelf_configuration(&defaults)
    }
}

fn decode_item(
    raw: (String, Option<i64>, i64, bool, i64),
) -> StorageResult<ShelfConfigurationItem> {
    let shelf_type = ShelfType::parse(&raw.0)?;
    let position = u32::try_from(raw.2)
        .map_err(|_| StorageError::InvalidSchema("shelf position 超出範圍".to_owned()))?;
    let preview_limit = u32::try_from(raw.4)
        .map_err(|_| StorageError::InvalidSchema("shelf preview limit 超出範圍".to_owned()))?;
    Ok(ShelfConfigurationItem {
        shelf_type,
        saved_view_id: raw.1,
        position,
        enabled: raw.3,
        preview_limit,
    })
}

fn validate_items(items: &[ShelfConfigurationItem]) -> StorageResult<()> {
    let mut builtins = HashSet::new();
    let mut saved_view_ids = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        if item.position != index as u32 {
            return invalid("position 必須從 0 起連續排列");
        }
        if !SHELF_PREVIEW_LIMIT_CHOICES.contains(&item.preview_limit) {
            return invalid("preview limit 必須是 6、8、12 或 16");
        }
        match (item.shelf_type, item.saved_view_id) {
            (ShelfType::SavedView, Some(saved_view_id)) => {
                if !saved_view_ids.insert(saved_view_id) {
                    return invalid("同一個 Saved View 不可重複加入首頁書架");
                }
            }
            (ShelfType::SavedView, None) => {
                return invalid("Saved View shelf 必須指定 saved view ID");
            }
            (shelf_type, Some(_)) if shelf_type.is_builtin() => {
                return invalid("built-in shelf 不可指定 saved view ID");
            }
            _ => {
                if !builtins.insert(item.shelf_type.as_str()) {
                    return invalid("每個 built-in shelf 必須恰有一筆");
                }
            }
        }
    }
    if builtins.len() != 3 {
        return invalid("recent、featured、event 三個 built-in shelves 都必須各有一筆");
    }
    Ok(())
}

fn invalid<T>(reason: &str) -> StorageResult<T> {
    Err(StorageError::InvalidShelfConfiguration(reason.to_owned()))
}
