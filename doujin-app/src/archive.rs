//! 「下載區 → 歸檔區」move 的唯讀 preflight。

use doujin_files::{ArchiveMoveReadiness, RecycleBin, plan_archive_move, validate_archive_root};
use doujin_storage::StorageError;
use serde::Serialize;

use crate::{ApplicationError, ApplicationResult, ApplicationService};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveMovePreflightStatus {
    Ready,
    ReadyUnclassified,
    NotDownloads,
    SourceMissing,
    Collision,
    Blocked,
    CollectionMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveMovePreflightItem {
    pub collection_id: i64,
    pub status: ArchiveMovePreflightStatus,
    pub destination: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ArchiveMovePreflightSummary {
    pub total: usize,
    pub ready: usize,
    pub ready_unclassified: usize,
    pub not_downloads: usize,
    pub source_missing: usize,
    pub collision: usize,
    pub blocked: usize,
    pub collection_missing: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveMovePreflight {
    pub archive_root_id: i64,
    pub archive_root_path: String,
    pub summary: ArchiveMovePreflightSummary,
    pub items: Vec<ArchiveMovePreflightItem>,
}

impl<R: RecycleBin> ApplicationService<R> {
    /// 回報每筆收藏的歸檔 readiness 與目的地預覽；全程唯讀，不執行任何檔案操作。
    pub fn move_preflight(
        &self,
        collection_ids: &[i64],
        archive_root_id: i64,
    ) -> ApplicationResult<ArchiveMovePreflight> {
        let archive_root = self.repository.library_root(archive_root_id)?;
        validate_archive_root(&archive_root)
            .map_err(|error| ApplicationError::InvalidSettings(error.to_string()))?;
        if !archive_root.path.is_dir() {
            return Err(ApplicationError::InvalidSettings(format!(
                "歸檔區路徑不存在或不是資料夾：{}",
                archive_root.path.display()
            )));
        }
        let mut items = Vec::with_capacity(collection_ids.len());
        for collection_id in collection_ids {
            items.push(
                match plan_archive_move(&self.repository, *collection_id, &archive_root) {
                    Ok(plan) => ArchiveMovePreflightItem {
                        collection_id: *collection_id,
                        status: match plan.readiness {
                            ArchiveMoveReadiness::Ready => ArchiveMovePreflightStatus::Ready,
                            ArchiveMoveReadiness::ReadyUnclassified => {
                                ArchiveMovePreflightStatus::ReadyUnclassified
                            }
                            ArchiveMoveReadiness::NotDownloads => {
                                ArchiveMovePreflightStatus::NotDownloads
                            }
                            ArchiveMoveReadiness::SourceMissing => {
                                ArchiveMovePreflightStatus::SourceMissing
                            }
                            ArchiveMoveReadiness::Collision => {
                                ArchiveMovePreflightStatus::Collision
                            }
                            ArchiveMoveReadiness::Blocked => ArchiveMovePreflightStatus::Blocked,
                        },
                        destination: plan
                            .destination
                            .map(|path| path.to_string_lossy().into_owned()),
                        message: plan.blocker.map(|error| error.to_string()),
                    },
                    Err(StorageError::CollectionNotFound(_)) => ArchiveMovePreflightItem {
                        collection_id: *collection_id,
                        status: ArchiveMovePreflightStatus::CollectionMissing,
                        destination: None,
                        message: Some("找不到指定的收藏，可能已刪除或合併".to_owned()),
                    },
                    Err(error) => return Err(error.into()),
                },
            );
        }
        Ok(ArchiveMovePreflight {
            archive_root_id,
            archive_root_path: archive_root.path.to_string_lossy().into_owned(),
            summary: summarize(&items),
            items,
        })
    }
}

fn summarize(items: &[ArchiveMovePreflightItem]) -> ArchiveMovePreflightSummary {
    let mut summary = ArchiveMovePreflightSummary {
        total: items.len(),
        ..ArchiveMovePreflightSummary::default()
    };
    for item in items {
        match item.status {
            ArchiveMovePreflightStatus::Ready => summary.ready += 1,
            ArchiveMovePreflightStatus::ReadyUnclassified => summary.ready_unclassified += 1,
            ArchiveMovePreflightStatus::NotDownloads => summary.not_downloads += 1,
            ArchiveMovePreflightStatus::SourceMissing => summary.source_missing += 1,
            ArchiveMovePreflightStatus::Collision => summary.collision += 1,
            ArchiveMovePreflightStatus::Blocked => summary.blocked += 1,
            ArchiveMovePreflightStatus::CollectionMissing => summary.collection_missing += 1,
        }
    }
    summary
}
