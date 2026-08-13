//! Persistent fingerprint cache and bounded duplicate-scan work queue.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use doujin_parser::domain::Identifier;
use rusqlite::{OptionalExtension, Transaction, params};
use unicode_normalization::UnicodeNormalization;

use crate::collections::CollectionSnapshot;
use crate::{CatalogRepository, StorageError, StorageResult};

pub const DUPLICATE_FINGERPRINT_ALGORITHM_VERSION: &str = "sha256-pages-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateScanStatus {
    Running,
    Completed,
    CompletedWithErrors,
}

impl DuplicateScanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::CompletedWithErrors => "completed_with_errors",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "completed_with_errors" => Ok(Self::CompletedWithErrors),
            value => Err(StorageError::InvalidSchema(format!(
                "未知 duplicate scan status：{value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateScanItemStatus {
    Pending,
    Running,
    Processed,
    Failed,
}

impl DuplicateScanItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Processed => "processed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "processed" => Ok(Self::Processed),
            "failed" => Ok(Self::Failed),
            value => Err(StorageError::InvalidSchema(format!(
                "未知 duplicate scan item status：{value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateScanJobSnapshot {
    pub id: i64,
    pub status: DuplicateScanStatus,
    pub total: usize,
    pub pending: usize,
    pub running: usize,
    pub processed: usize,
    pub failed: usize,
    pub reused_cache: usize,
    pub concurrency_limit: usize,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateScanItemSnapshot {
    pub job_id: i64,
    pub collection_id: i64,
    pub path: std::path::PathBuf,
    pub status: DuplicateScanItemStatus,
    pub attempts: usize,
    pub reused_cache: bool,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateFingerprint {
    pub collection_id: i64,
    pub source_fingerprint: String,
    pub algorithm_version: String,
    pub source_size: u64,
    pub file_sha256: Option<String>,
    pub archive_entry_count: usize,
    pub image_count: usize,
    pub content_fingerprint: String,
    pub page_hashes: Vec<String>,
    pub perceptual_hashes: Option<Vec<String>>,
    pub calculated_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DuplicateLevel {
    Probable,
    Content,
    Exact,
}

impl DuplicateLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Content => "content",
            Self::Probable => "probable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCollectionEvidence {
    pub collection: CollectionSnapshot,
    pub file_size: u64,
    pub page_count: usize,
    pub archive_entry_count: usize,
    pub fingerprint_identity: String,
    pub metadata_completeness: usize,
    pub tag_count: usize,
    pub manual_assertion_count: usize,
    pub identifiers: Vec<String>,
    pub max_image_width: Option<u32>,
    pub max_image_height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateCandidatePair {
    pub left: DuplicateCollectionEvidence,
    pub right: DuplicateCollectionEvidence,
    pub level: DuplicateLevel,
    pub confidence: f64,
    pub reasons: Vec<String>,
    pub matching_pages: usize,
    pub compared_pages: usize,
    pub reviewed: bool,
}

impl DuplicateFingerprint {
    pub fn identity(&self) -> String {
        format!(
            "{}:{}:{}",
            self.algorithm_version, self.source_fingerprint, self.content_fingerprint
        )
    }
}

impl CatalogRepository {
    pub fn duplicate_candidates(&self) -> StorageResult<Vec<DuplicateCandidatePair>> {
        let mut statement = self.connection.prepare(
            "SELECT fingerprint.collection_id
             FROM duplicate_fingerprints AS fingerprint
             JOIN collections AS collection ON collection.id = fingerprint.collection_id
             WHERE collection.status = 'active'
               AND EXISTS (
                   SELECT 1 FROM collection_locations AS location
                   WHERE location.collection_id = collection.id
                     AND location.location_status = 'current'
               )
             ORDER BY fingerprint.collection_id",
        )?;
        let collection_ids = statement
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let entries = collection_ids
            .into_iter()
            .map(|collection_id| self.duplicate_candidate_entry(collection_id))
            .collect::<StorageResult<Vec<_>>>()?;
        let mut candidates = BTreeMap::<(i64, i64), DuplicateCandidatePair>::new();

        add_group_pairs(
            self,
            &entries,
            |entry| {
                entry
                    .fingerprint
                    .file_sha256
                    .clone()
                    .map(|sha| (sha, entry.fingerprint.source_size))
            },
            DuplicateLevel::Exact,
            &mut candidates,
            |left, right| {
                let pages = matching_page_count(
                    &left.fingerprint.page_hashes,
                    &right.fingerprint.page_hashes,
                );
                PairEvidence {
                    confidence: 1.0,
                    reasons: vec!["SHA-256 與檔案大小完全相同".to_owned()],
                    matching_pages: pages,
                    compared_pages: left
                        .fingerprint
                        .image_count
                        .max(right.fingerprint.image_count),
                }
            },
        )?;
        add_group_pairs(
            self,
            &entries,
            |entry| Some(entry.fingerprint.content_fingerprint.clone()),
            DuplicateLevel::Content,
            &mut candidates,
            |left, right| PairEvidence {
                confidence: 0.99,
                reasons: vec![format!(
                    "{} / {} 頁內容 hash 相同；archive SHA-256 不同",
                    left.fingerprint.image_count,
                    left.fingerprint
                        .image_count
                        .max(right.fingerprint.image_count)
                )],
                matching_pages: left.fingerprint.image_count,
                compared_pages: left
                    .fingerprint
                    .image_count
                    .max(right.fingerprint.image_count),
            },
        )?;

        // A nearly identical set of byte-exact pages is strong content evidence,
        // even if a small scanner/credit page differs. This never triggers deletion.
        for left_index in 0..entries.len() {
            for right_index in (left_index + 1)..entries.len() {
                let left = &entries[left_index];
                let right = &entries[right_index];
                let key = pair_key(left.collection.id, right.collection.id);
                if candidates.contains_key(&key) {
                    continue;
                }
                let matching = matching_page_count(
                    &left.fingerprint.page_hashes,
                    &right.fingerprint.page_hashes,
                );
                let compared = left
                    .fingerprint
                    .image_count
                    .max(right.fingerprint.image_count);
                if compared >= 10 && matching * 100 >= compared * 95 {
                    insert_pair(
                        self,
                        left,
                        right,
                        DuplicateLevel::Content,
                        PairEvidence {
                            confidence: 0.96,
                            reasons: vec![format!(
                                "{matching} / {compared} 頁為 byte-exact match；少量頁面不同，需人工裁決"
                            )],
                            matching_pages: matching,
                            compared_pages: compared,
                        },
                        &mut candidates,
                    )?;
                }
            }
        }

        let mut identifier_groups = HashMap::<String, Vec<usize>>::new();
        for (index, entry) in entries.iter().enumerate() {
            for identifier in &entry.identifiers {
                identifier_groups
                    .entry(identifier_key(identifier))
                    .or_default()
                    .push(index);
            }
        }
        for (identifier, indexes) in identifier_groups {
            for_each_pair(&indexes, |left_index, right_index| {
                let left = &entries[left_index];
                let right = &entries[right_index];
                let key = pair_key(left.collection.id, right.collection.id);
                if candidates.contains_key(&key) {
                    return Ok(());
                }
                let (matching, compared) = page_match_counts(left, right);
                insert_pair(
                    self,
                    left,
                    right,
                    DuplicateLevel::Probable,
                    PairEvidence {
                        confidence: 0.92,
                        reasons: vec![format!("可靠 identifier 完全相同：{identifier}")],
                        matching_pages: matching,
                        compared_pages: compared,
                    },
                    &mut candidates,
                )
            })?;
        }

        let mut title_groups = HashMap::<String, Vec<usize>>::new();
        for (index, entry) in entries.iter().enumerate() {
            let title = entry.collection.title.as_deref().unwrap_or_default();
            let normalized = normalized_text(title);
            if !normalized.is_empty() {
                title_groups.entry(normalized).or_default().push(index);
            }
        }
        for indexes in title_groups.into_values() {
            for_each_pair(&indexes, |left_index, right_index| {
                let left = &entries[left_index];
                let right = &entries[right_index];
                let key = pair_key(left.collection.id, right.collection.id);
                if candidates.contains_key(&key) || !pages_are_close(left, right) {
                    return Ok(());
                }
                let same_circle = same_nonempty(
                    left.collection.circle.as_deref(),
                    right.collection.circle.as_deref(),
                );
                let shared_author = normalized_values(&left.collection.authors)
                    .intersection(&normalized_values(&right.collection.authors))
                    .next()
                    .is_some();
                if !same_circle && !shared_author {
                    return Ok(());
                }
                let mut reasons = vec!["正規化標題完全相同且頁數接近".to_owned()];
                if same_circle {
                    reasons.push("社團相同".to_owned());
                }
                if shared_author {
                    reasons.push("至少一位作者相同".to_owned());
                }
                let (matching, compared) = page_match_counts(left, right);
                insert_pair(
                    self,
                    left,
                    right,
                    DuplicateLevel::Probable,
                    PairEvidence {
                        confidence: if same_circle && shared_author {
                            0.88
                        } else {
                            0.82
                        },
                        reasons,
                        matching_pages: matching,
                        compared_pages: compared,
                    },
                    &mut candidates,
                )
            })?;
        }

        let mut candidates = candidates.into_values().collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .level
                .cmp(&left.level)
                .then_with(|| {
                    right
                        .confidence
                        .partial_cmp(&left.confidence)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| left.left.collection.id.cmp(&right.left.collection.id))
                .then_with(|| left.right.collection.id.cmp(&right.right.collection.id))
        });
        Ok(candidates)
    }

    fn duplicate_candidate_entry(&self, collection_id: i64) -> StorageResult<CandidateEntry> {
        let collection = self.collection(collection_id)?;
        let fingerprint = self.duplicate_fingerprint(collection_id)?.ok_or_else(|| {
            StorageError::InvalidSchema(format!("收藏 {collection_id} 缺少 duplicate fingerprint"))
        })?;
        let identifiers = self.latest_parser_identifiers(collection_id)?;
        let manual_assertion_count = self.connection.query_row(
            "SELECT count(*) FROM metadata_assertions
             WHERE collection_id = ?1 AND source_kind = 'manual'
               AND status IN ('candidate', 'accepted')",
            [collection_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(CandidateEntry {
            metadata_completeness: metadata_completeness(&collection),
            manual_assertion_count: count(manual_assertion_count)?,
            identifiers,
            collection,
            fingerprint,
        })
    }

    pub fn create_duplicate_scan_job(
        &mut self,
        collection_ids: &[i64],
        concurrency_limit: usize,
    ) -> StorageResult<DuplicateScanJobSnapshot> {
        if !(1..=8).contains(&concurrency_limit) {
            return Err(StorageError::InvalidLifecycle(
                "duplicate scan concurrency 必須介於 1 到 8".to_owned(),
            ));
        }
        let mut collection_ids = collection_ids.to_vec();
        collection_ids.sort_unstable();
        collection_ids.dedup();
        if collection_ids.iter().any(|id| *id <= 0) {
            return Err(StorageError::InvalidLifecycle(
                "duplicate scan collection ID 必須是正整數".to_owned(),
            ));
        }
        let transaction = self.connection.transaction()?;
        let running = transaction
            .query_row(
                "SELECT id FROM duplicate_scan_jobs WHERE status = 'running' LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if running.is_some() {
            return Err(StorageError::InvalidLifecycle(
                "已有 duplicate scan job 正在執行".to_owned(),
            ));
        }
        for collection_id in &collection_ids {
            super::ensure_collection(&transaction, *collection_id)?;
        }
        transaction.execute(
            "INSERT INTO duplicate_scan_jobs(status, total, concurrency_limit)
             VALUES ('running', ?1, ?2)",
            params![collection_ids.len() as i64, concurrency_limit as i64],
        )?;
        let job_id = transaction.last_insert_rowid();
        for collection_id in collection_ids {
            transaction.execute(
                "INSERT INTO duplicate_scan_items(job_id, collection_id)
                 VALUES (?1, ?2)",
                params![job_id, collection_id],
            )?;
        }
        if transaction.query_row(
            "SELECT total FROM duplicate_scan_jobs WHERE id = ?1",
            [job_id],
            |row| row.get::<_, i64>(0),
        )? == 0
        {
            transaction.execute(
                "UPDATE duplicate_scan_jobs
                 SET status = 'completed', completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ?1",
                [job_id],
            )?;
        }
        transaction.commit()?;
        self.duplicate_scan_job(job_id)
    }

    pub fn duplicate_scan_job(&self, job_id: i64) -> StorageResult<DuplicateScanJobSnapshot> {
        let raw = self
            .connection
            .query_row(
                "SELECT job.id, job.status, job.total, job.concurrency_limit,
                    job.created_at, job.updated_at, job.completed_at,
                    sum(CASE WHEN item.status = 'pending' THEN 1 ELSE 0 END),
                    sum(CASE WHEN item.status = 'running' THEN 1 ELSE 0 END),
                    sum(CASE WHEN item.status = 'processed' THEN 1 ELSE 0 END),
                    sum(CASE WHEN item.status = 'failed' THEN 1 ELSE 0 END),
                    sum(CASE WHEN item.reused_cache = 1 THEN 1 ELSE 0 END)
             FROM duplicate_scan_jobs AS job
             LEFT JOIN duplicate_scan_items AS item ON item.job_id = job.id
             WHERE job.id = ?1 GROUP BY job.id",
                [job_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, i64>(11)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::InvalidLifecycle(format!("找不到 duplicate scan job ID：{job_id}"))
            })?;
        Ok(DuplicateScanJobSnapshot {
            id: raw.0,
            status: DuplicateScanStatus::parse(&raw.1)?,
            total: count(raw.2)?,
            concurrency_limit: count(raw.3)?,
            created_at: raw.4,
            updated_at: raw.5,
            completed_at: raw.6,
            pending: count(raw.7)?,
            running: count(raw.8)?,
            processed: count(raw.9)?,
            failed: count(raw.10)?,
            reused_cache: count(raw.11)?,
        })
    }

    pub fn latest_duplicate_scan_job(&self) -> StorageResult<Option<DuplicateScanJobSnapshot>> {
        let job_id = self
            .connection
            .query_row(
                "SELECT id FROM duplicate_scan_jobs ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        job_id
            .map(|job_id| self.duplicate_scan_job(job_id))
            .transpose()
    }

    pub fn duplicate_scan_failures(
        &self,
        job_id: i64,
    ) -> StorageResult<Vec<DuplicateScanItemSnapshot>> {
        let mut statement = self.connection.prepare(
            "SELECT item.job_id, item.collection_id, location.full_path, item.status,
                    item.attempts, item.reused_cache, item.error_kind, item.error_message
             FROM duplicate_scan_items AS item
             LEFT JOIN collection_locations AS location ON location.collection_id = item.collection_id
                AND location.location_status = 'current'
             WHERE item.job_id = ?1 AND item.status = 'failed'
             ORDER BY item.collection_id",
        )?;
        statement
            .query_map([job_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?
            .map(|row| {
                let row = row?;
                Ok(DuplicateScanItemSnapshot {
                    job_id: row.0,
                    collection_id: row.1,
                    path: row.2.map(Into::into).unwrap_or_default(),
                    status: DuplicateScanItemStatus::parse(&row.3)?,
                    attempts: count(row.4)?,
                    reused_cache: row.5,
                    error_kind: row.6,
                    error_message: row.7,
                })
            })
            .collect()
    }

    pub fn claim_duplicate_scan_item(
        &mut self,
    ) -> StorageResult<Option<DuplicateScanItemSnapshot>> {
        let transaction = self.connection.transaction()?;
        let row = transaction
            .query_row(
                "SELECT item.job_id, item.collection_id, location.full_path, item.status,
                    item.attempts, item.reused_cache, item.error_kind, item.error_message
             FROM duplicate_scan_items AS item
             JOIN duplicate_scan_jobs AS job ON job.id = item.job_id AND job.status = 'running'
             JOIN collections AS collection ON collection.id = item.collection_id
             JOIN collection_locations AS location ON location.collection_id = collection.id
                AND location.location_status = 'current'
             WHERE item.status = 'pending'
               AND (SELECT count(*) FROM duplicate_scan_items AS active
                    WHERE active.job_id = item.job_id AND active.status = 'running')
                   < job.concurrency_limit
             ORDER BY item.job_id, item.collection_id LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some(row) = row else {
            transaction.commit()?;
            return Ok(None);
        };
        transaction.execute(
            "UPDATE duplicate_scan_items
             SET status = 'running', attempts = attempts + 1,
                 started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE job_id = ?1 AND collection_id = ?2 AND status = 'pending'",
            params![row.0, row.1],
        )?;
        transaction.commit()?;
        Ok(Some(DuplicateScanItemSnapshot {
            job_id: row.0,
            collection_id: row.1,
            path: row.2.into(),
            status: DuplicateScanItemStatus::Running,
            attempts: count(row.4 + 1)?,
            reused_cache: row.5,
            error_kind: row.6,
            error_message: row.7,
        }))
    }

    pub fn upsert_duplicate_fingerprint(
        &mut self,
        fingerprint: &DuplicateFingerprint,
    ) -> StorageResult<()> {
        let transaction = self.connection.transaction()?;
        upsert_fingerprint(&transaction, fingerprint)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn duplicate_fingerprint(
        &self,
        collection_id: i64,
    ) -> StorageResult<Option<DuplicateFingerprint>> {
        let raw = self
            .connection
            .query_row(
                "SELECT collection_id, source_fingerprint, algorithm_version, source_size,
                    file_sha256, archive_entry_count, image_count, content_fingerprint,
                    page_hashes_json, perceptual_hashes_json, calculated_at
             FROM duplicate_fingerprints WHERE collection_id = ?1",
                [collection_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, String>(10)?,
                    ))
                },
            )
            .optional()?;
        raw.map(|raw| {
            Ok(DuplicateFingerprint {
                collection_id: raw.0,
                source_fingerprint: raw.1,
                algorithm_version: raw.2,
                source_size: u64::try_from(raw.3).map_err(|_| invalid_count(raw.3))?,
                file_sha256: raw.4,
                archive_entry_count: count(raw.5)?,
                image_count: count(raw.6)?,
                content_fingerprint: raw.7,
                page_hashes: serde_json::from_str(&raw.8)?,
                perceptual_hashes: raw.9.as_deref().map(serde_json::from_str).transpose()?,
                calculated_at: Some(raw.10),
            })
        })
        .transpose()
    }

    pub fn cached_duplicate_fingerprint(
        &self,
        collection_id: i64,
        source_fingerprint: &str,
        algorithm_version: &str,
    ) -> StorageResult<Option<DuplicateFingerprint>> {
        let fingerprint = self.duplicate_fingerprint(collection_id)?;
        Ok(fingerprint.filter(|fingerprint| {
            fingerprint.source_fingerprint == source_fingerprint
                && fingerprint.algorithm_version == algorithm_version
        }))
    }

    pub fn complete_duplicate_scan_item(
        &mut self,
        job_id: i64,
        fingerprint: &DuplicateFingerprint,
        reused_cache: bool,
    ) -> StorageResult<DuplicateScanJobSnapshot> {
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE duplicate_scan_items
             SET status = 'processed', reused_cache = ?3, error_kind = NULL,
                 error_message = NULL,
                 completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE job_id = ?1 AND collection_id = ?2 AND status = 'running'",
            params![job_id, fingerprint.collection_id, reused_cache],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidLifecycle(format!(
                "duplicate scan item {job_id}/{} 目前不可完成",
                fingerprint.collection_id
            )));
        }
        upsert_fingerprint(&transaction, fingerprint)?;
        finalize_job(&transaction, job_id)?;
        transaction.commit()?;
        self.duplicate_scan_job(job_id)
    }

    pub fn fail_duplicate_scan_item(
        &mut self,
        job_id: i64,
        collection_id: i64,
        error_kind: &str,
        error_message: &str,
    ) -> StorageResult<DuplicateScanJobSnapshot> {
        if error_kind.trim().is_empty() || error_message.trim().is_empty() {
            return Err(StorageError::InvalidLifecycle(
                "duplicate fingerprint failure 必須包含 kind 與 message".to_owned(),
            ));
        }
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE duplicate_scan_items
             SET status = 'failed', error_kind = ?3, error_message = ?4,
                 completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE job_id = ?1 AND collection_id = ?2 AND status = 'running'",
            params![job_id, collection_id, error_kind, error_message],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidLifecycle(format!(
                "duplicate scan item {job_id}/{collection_id} 目前不可標記失敗"
            )));
        }
        finalize_job(&transaction, job_id)?;
        transaction.commit()?;
        self.duplicate_scan_job(job_id)
    }

    pub fn retry_failed_duplicate_scan_items(
        &mut self,
        job_id: i64,
    ) -> StorageResult<DuplicateScanJobSnapshot> {
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE duplicate_scan_items
             SET status = 'pending', reused_cache = 0, error_kind = NULL,
                 error_message = NULL, started_at = NULL, completed_at = NULL
             WHERE job_id = ?1 AND status = 'failed'",
            [job_id],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidLifecycle(
                "duplicate scan job 沒有可重試的失敗項目".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE duplicate_scan_jobs
             SET status = 'running', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                 completed_at = NULL WHERE id = ?1",
            [job_id],
        )?;
        transaction.commit()?;
        self.duplicate_scan_job(job_id)
    }

    pub fn recover_interrupted_duplicate_scan_items(&mut self) -> StorageResult<usize> {
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE duplicate_scan_items
             SET status = 'pending', started_at = NULL
             WHERE status = 'running'",
            [],
        )?;
        transaction.execute(
            "UPDATE duplicate_scan_jobs
             SET status = 'running', completed_at = NULL,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id IN (
                 SELECT DISTINCT job_id FROM duplicate_scan_items WHERE status = 'pending'
             )",
            [],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn exclude_duplicate_pair(
        &mut self,
        first_collection_id: i64,
        first_identity: &str,
        second_collection_id: i64,
        second_identity: &str,
    ) -> StorageResult<()> {
        let (left_id, left_identity, right_id, right_identity) = canonical_pair(
            first_collection_id,
            first_identity,
            second_collection_id,
            second_identity,
        )?;
        let transaction = self.connection.transaction()?;
        super::ensure_collection(&transaction, left_id)?;
        super::ensure_collection(&transaction, right_id)?;
        transaction.execute(
            "INSERT INTO duplicate_exclusions(
                 left_collection_id, right_collection_id,
                 left_fingerprint_identity, right_fingerprint_identity
             ) VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
            params![left_id, right_id, left_identity, right_identity],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn duplicate_pair_is_excluded(
        &self,
        first_collection_id: i64,
        first_identity: &str,
        second_collection_id: i64,
        second_identity: &str,
    ) -> StorageResult<bool> {
        let (left_id, left_identity, right_id, right_identity) = canonical_pair(
            first_collection_id,
            first_identity,
            second_collection_id,
            second_identity,
        )?;
        Ok(self.connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM duplicate_exclusions
                 WHERE left_collection_id = ?1 AND right_collection_id = ?2
                   AND left_fingerprint_identity = ?3 AND right_fingerprint_identity = ?4
             )",
            params![left_id, right_id, left_identity, right_identity],
            |row| row.get(0),
        )?)
    }

    pub fn confirm_duplicate_pair(
        &mut self,
        first_collection_id: i64,
        first_identity: &str,
        second_collection_id: i64,
        second_identity: &str,
    ) -> StorageResult<()> {
        let (left_id, left_identity, right_id, right_identity) = canonical_pair(
            first_collection_id,
            first_identity,
            second_collection_id,
            second_identity,
        )?;
        let transaction = self.connection.transaction()?;
        super::ensure_collection(&transaction, left_id)?;
        super::ensure_collection(&transaction, right_id)?;
        transaction.execute(
            "INSERT INTO duplicate_reviews(
                 left_collection_id, right_collection_id,
                 left_fingerprint_identity, right_fingerprint_identity, decision
             ) VALUES (?1, ?2, ?3, ?4, 'confirmed_duplicate') ON CONFLICT DO NOTHING",
            params![left_id, right_id, left_identity, right_identity],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn duplicate_pair_is_confirmed(
        &self,
        first_collection_id: i64,
        first_identity: &str,
        second_collection_id: i64,
        second_identity: &str,
    ) -> StorageResult<bool> {
        let (left_id, left_identity, right_id, right_identity) = canonical_pair(
            first_collection_id,
            first_identity,
            second_collection_id,
            second_identity,
        )?;
        Ok(self.connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM duplicate_reviews
                 WHERE left_collection_id = ?1 AND right_collection_id = ?2
                   AND left_fingerprint_identity = ?3 AND right_fingerprint_identity = ?4
                   AND decision = 'confirmed_duplicate'
             )",
            params![left_id, right_id, left_identity, right_identity],
            |row| row.get(0),
        )?)
    }
}

#[derive(Debug)]
struct CandidateEntry {
    collection: CollectionSnapshot,
    fingerprint: DuplicateFingerprint,
    metadata_completeness: usize,
    manual_assertion_count: usize,
    identifiers: Vec<Identifier>,
}

struct PairEvidence {
    confidence: f64,
    reasons: Vec<String>,
    matching_pages: usize,
    compared_pages: usize,
}

fn add_group_pairs<K, F, E>(
    repository: &CatalogRepository,
    entries: &[CandidateEntry],
    key: F,
    level: DuplicateLevel,
    candidates: &mut BTreeMap<(i64, i64), DuplicateCandidatePair>,
    evidence: E,
) -> StorageResult<()>
where
    K: Eq + std::hash::Hash,
    F: Fn(&CandidateEntry) -> Option<K>,
    E: Fn(&CandidateEntry, &CandidateEntry) -> PairEvidence,
{
    let mut groups = HashMap::<K, Vec<usize>>::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(key) = key(entry) {
            groups.entry(key).or_default().push(index);
        }
    }
    for indexes in groups.into_values() {
        for_each_pair(&indexes, |left_index, right_index| {
            let left = &entries[left_index];
            let right = &entries[right_index];
            insert_pair(
                repository,
                left,
                right,
                level,
                evidence(left, right),
                candidates,
            )
        })?;
    }
    Ok(())
}

fn insert_pair(
    repository: &CatalogRepository,
    left: &CandidateEntry,
    right: &CandidateEntry,
    level: DuplicateLevel,
    evidence: PairEvidence,
    candidates: &mut BTreeMap<(i64, i64), DuplicateCandidatePair>,
) -> StorageResult<()> {
    let (left, right) = if left.collection.id < right.collection.id {
        (left, right)
    } else {
        (right, left)
    };
    let key = (left.collection.id, right.collection.id);
    if candidates
        .get(&key)
        .is_some_and(|candidate| candidate.level >= level)
    {
        return Ok(());
    }
    let left_identity = left.fingerprint.identity();
    let right_identity = right.fingerprint.identity();
    if repository.duplicate_pair_is_excluded(
        left.collection.id,
        &left_identity,
        right.collection.id,
        &right_identity,
    )? {
        return Ok(());
    }
    let reviewed = repository.duplicate_pair_is_confirmed(
        left.collection.id,
        &left_identity,
        right.collection.id,
        &right_identity,
    )?;
    candidates.insert(
        key,
        DuplicateCandidatePair {
            left: collection_evidence(left),
            right: collection_evidence(right),
            level,
            confidence: evidence.confidence,
            reasons: evidence.reasons,
            matching_pages: evidence.matching_pages,
            compared_pages: evidence.compared_pages,
            reviewed,
        },
    );
    Ok(())
}

fn collection_evidence(entry: &CandidateEntry) -> DuplicateCollectionEvidence {
    DuplicateCollectionEvidence {
        file_size: entry.fingerprint.source_size,
        page_count: entry.fingerprint.image_count,
        archive_entry_count: entry.fingerprint.archive_entry_count,
        fingerprint_identity: entry.fingerprint.identity(),
        metadata_completeness: entry.metadata_completeness,
        tag_count: entry.collection.tags.len(),
        manual_assertion_count: entry.manual_assertion_count,
        identifiers: entry
            .identifiers
            .iter()
            .map(|identifier| format!("{}:{}", identifier.scheme, identifier.value))
            .collect(),
        max_image_width: None,
        max_image_height: None,
        collection: entry.collection.clone(),
    }
}

fn for_each_pair<F>(indexes: &[usize], mut function: F) -> StorageResult<()>
where
    F: FnMut(usize, usize) -> StorageResult<()>,
{
    for left in 0..indexes.len() {
        for right in (left + 1)..indexes.len() {
            function(indexes[left], indexes[right])?;
        }
    }
    Ok(())
}

fn pair_key(first: i64, second: i64) -> (i64, i64) {
    if first < second {
        (first, second)
    } else {
        (second, first)
    }
}

fn identifier_key(identifier: &Identifier) -> String {
    format!(
        "{}:{}",
        identifier.scheme.trim().to_uppercase(),
        identifier.value.trim().to_uppercase()
    )
}

fn metadata_completeness(collection: &CollectionSnapshot) -> usize {
    [
        collection.title.as_deref(),
        collection.event.as_deref(),
        collection.circle.as_deref(),
        collection.parody.as_deref(),
        collection.classification_top.as_deref(),
    ]
    .into_iter()
    .filter(|value| value.is_some_and(|value| !value.trim().is_empty()))
    .count()
        + usize::from(!collection.authors.is_empty())
}

fn normalized_text(value: &str) -> String {
    value
        .nfkc()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn normalized_values(values: &[String]) -> HashSet<String> {
    values
        .iter()
        .map(|value| normalized_text(value))
        .filter(|value| !value.is_empty())
        .collect()
}

fn same_nonempty(left: Option<&str>, right: Option<&str>) -> bool {
    let left = left.map(normalized_text).unwrap_or_default();
    !left.is_empty() && left == right.map(normalized_text).unwrap_or_default()
}

fn pages_are_close(left: &CandidateEntry, right: &CandidateEntry) -> bool {
    let left = left.fingerprint.image_count;
    let right = right.fingerprint.image_count;
    let difference = left.abs_diff(right);
    difference <= 2 || difference * 10 <= left.max(right).max(1)
}

fn page_match_counts(left: &CandidateEntry, right: &CandidateEntry) -> (usize, usize) {
    (
        matching_page_count(
            &left.fingerprint.page_hashes,
            &right.fingerprint.page_hashes,
        ),
        left.fingerprint
            .image_count
            .max(right.fingerprint.image_count),
    )
}

fn matching_page_count(left: &[String], right: &[String]) -> usize {
    let mut counts = HashMap::<&str, usize>::new();
    for hash in left {
        *counts.entry(hash).or_default() += 1;
    }
    let mut matching = 0;
    for hash in right {
        if let Some(count) = counts.get_mut(hash.as_str())
            && *count > 0
        {
            *count -= 1;
            matching += 1;
        }
    }
    matching
}

fn upsert_fingerprint(
    transaction: &Transaction<'_>,
    fingerprint: &DuplicateFingerprint,
) -> StorageResult<()> {
    super::ensure_collection(transaction, fingerprint.collection_id)?;
    let page_hashes = serde_json::to_string(&fingerprint.page_hashes)?;
    let perceptual_hashes = fingerprint
        .perceptual_hashes
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    transaction.execute(
        "INSERT INTO duplicate_fingerprints(
             collection_id, source_fingerprint, algorithm_version, source_size,
             file_sha256, archive_entry_count, image_count, content_fingerprint,
             page_hashes_json, perceptual_hashes_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(collection_id) DO UPDATE SET
             source_fingerprint = excluded.source_fingerprint,
             algorithm_version = excluded.algorithm_version,
             source_size = excluded.source_size,
             file_sha256 = excluded.file_sha256,
             archive_entry_count = excluded.archive_entry_count,
             image_count = excluded.image_count,
             content_fingerprint = excluded.content_fingerprint,
             page_hashes_json = excluded.page_hashes_json,
             perceptual_hashes_json = excluded.perceptual_hashes_json,
             calculated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        params![
            fingerprint.collection_id,
            fingerprint.source_fingerprint,
            fingerprint.algorithm_version,
            fingerprint.source_size as i64,
            fingerprint.file_sha256,
            fingerprint.archive_entry_count as i64,
            fingerprint.image_count as i64,
            fingerprint.content_fingerprint,
            page_hashes,
            perceptual_hashes,
        ],
    )?;
    Ok(())
}

fn finalize_job(transaction: &Transaction<'_>, job_id: i64) -> StorageResult<()> {
    let (pending, running, failed) = transaction.query_row(
        "SELECT
             sum(CASE WHEN status = 'pending' THEN 1 ELSE 0 END),
             sum(CASE WHEN status = 'running' THEN 1 ELSE 0 END),
             sum(CASE WHEN status = 'failed' THEN 1 ELSE 0 END)
         FROM duplicate_scan_items WHERE job_id = ?1",
        [job_id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    if pending == 0 && running == 0 {
        let status = if failed == 0 {
            "completed"
        } else {
            "completed_with_errors"
        };
        transaction.execute(
            "UPDATE duplicate_scan_jobs
             SET status = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                 completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![job_id, status],
        )?;
    } else {
        transaction.execute(
            "UPDATE duplicate_scan_jobs
             SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            [job_id],
        )?;
    }
    Ok(())
}

fn canonical_pair<'a>(
    first_id: i64,
    first_identity: &'a str,
    second_id: i64,
    second_identity: &'a str,
) -> StorageResult<(i64, &'a str, i64, &'a str)> {
    if first_id <= 0 || second_id <= 0 || first_id == second_id {
        return Err(StorageError::InvalidLifecycle(
            "duplicate pair 必須包含兩筆不同的正整數 collection ID".to_owned(),
        ));
    }
    if first_identity.trim().is_empty() || second_identity.trim().is_empty() {
        return Err(StorageError::InvalidLifecycle(
            "duplicate pair 必須包含雙方 fingerprint identity".to_owned(),
        ));
    }
    Ok(if first_id < second_id {
        (first_id, first_identity, second_id, second_identity)
    } else {
        (second_id, second_identity, first_id, first_identity)
    })
}

fn count(value: i64) -> StorageResult<usize> {
    usize::try_from(value).map_err(|_| invalid_count(value))
}

fn invalid_count(value: i64) -> StorageError {
    StorageError::InvalidSchema(format!("duplicate scan count 無效：{value}"))
}
