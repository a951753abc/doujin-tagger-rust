use std::path::PathBuf;

use doujin_scanner::MediaKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionStatus {
    Active,
    Tombstone,
    SoftDeleted,
}

impl CollectionStatus {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "active" => Ok(Self::Active),
            "tombstone" => Ok(Self::Tombstone),
            "soft_deleted" => Ok(Self::SoftDeleted),
            _ => Err(format!("未知 collection status：{value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationStatus {
    Current,
    Missing,
    Moved,
    Deleted,
}

impl LocationStatus {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "current" => Ok(Self::Current),
            "missing" => Ok(Self::Missing),
            "moved" => Ok(Self::Moved),
            "deleted" => Ok(Self::Deleted),
            _ => Err(format!("未知 location status：{value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationSnapshot {
    pub path: PathBuf,
    pub status: LocationStatus,
    pub root_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateDecision {
    Pending,
    Confirmed,
    Rejected,
}

impl CandidateDecision {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "confirmed" => Ok(Self::Confirmed),
            "rejected" => Ok(Self::Rejected),
            _ => Err(format!("未知 candidate decision：{value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneCandidateSnapshot {
    pub tombstone_collection_id: i64,
    pub candidate_collection_id: i64,
    pub tombstone_path: PathBuf,
    pub candidate_path: Option<PathBuf>,
    pub reason: String,
    pub decision: CandidateDecision,
    pub discovered_at: String,
    pub decided_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCollectionLocationSnapshot {
    pub collection_id: i64,
    pub path: PathBuf,
    pub root_path: PathBuf,
    pub media_kind: MediaKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    Soft,
    Permanent,
}

impl DeleteMode {
    pub(crate) fn operation_kind(self) -> &'static str {
        match self {
            Self::Soft => "soft_delete",
            Self::Permanent => "hard_delete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOperationKind {
    Rename,
    Move,
    SoftDelete,
    HardDelete,
}

impl FileOperationKind {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "rename" => Ok(Self::Rename),
            "move" => Ok(Self::Move),
            "soft_delete" => Ok(Self::SoftDelete),
            "hard_delete" => Ok(Self::HardDelete),
            _ => Err(format!("不支援的 file operation kind：{value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFileOperation {
    pub id: i64,
    pub collection_id: i64,
    pub kind: FileOperationKind,
    pub from_path: PathBuf,
    pub to_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOperationStatus {
    Pending,
    Succeeded,
    Failed,
}

impl FileOperationStatus {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => Err(format!("未知 file operation status：{value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOperationSnapshot {
    pub id: i64,
    pub collection_id: Option<i64>,
    pub kind: FileOperationKind,
    pub status: FileOperationStatus,
    pub from_path: PathBuf,
    pub to_path: Option<PathBuf>,
    pub error_message: Option<String>,
}
