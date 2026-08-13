//! Duplicate fingerprint background-work application boundary.

use std::path::PathBuf;

use doujin_files::RecycleBin;
use doujin_storage::duplicates::{
    DUPLICATE_FINGERPRINT_ALGORITHM_VERSION, DuplicateCandidatePair, DuplicateFingerprint,
    DuplicateScanItemSnapshot, DuplicateScanJobSnapshot,
};
use doujin_thumbnails::{SourceContentFingerprint, ThumbnailError, duplicate_source_fingerprint};

use crate::{ApplicationResult, ApplicationService};

pub const DUPLICATE_WORKER_COUNT: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateFingerprintRequest {
    pub job_id: i64,
    pub collection_id: i64,
    pub source_path: PathBuf,
}

impl<R: RecycleBin> ApplicationService<R> {
    pub fn start_duplicate_scan(&mut self) -> ApplicationResult<DuplicateScanJobSnapshot> {
        let collection_ids = self.repository.active_collection_ids()?;
        Ok(self
            .repository
            .create_duplicate_scan_job(&collection_ids, DUPLICATE_WORKER_COUNT)?)
    }

    pub fn duplicate_scan_job(&self, job_id: i64) -> ApplicationResult<DuplicateScanJobSnapshot> {
        Ok(self.repository.duplicate_scan_job(job_id)?)
    }

    pub fn latest_duplicate_scan_job(&self) -> ApplicationResult<Option<DuplicateScanJobSnapshot>> {
        Ok(self.repository.latest_duplicate_scan_job()?)
    }

    pub fn duplicate_scan_failures(
        &self,
        job_id: i64,
    ) -> ApplicationResult<Vec<DuplicateScanItemSnapshot>> {
        Ok(self.repository.duplicate_scan_failures(job_id)?)
    }

    /// Claims work while holding the repository writer, but performs only a
    /// metadata identity check. Full archive/image hashing happens after this
    /// method returns and therefore outside the shared application lock.
    pub fn claim_duplicate_fingerprint(
        &mut self,
    ) -> ApplicationResult<Option<DuplicateFingerprintRequest>> {
        loop {
            let Some(item) = self.repository.claim_duplicate_scan_item()? else {
                return Ok(None);
            };
            let cached = duplicate_source_fingerprint(&item.path)
                .ok()
                .and_then(|source_identity| {
                    self.repository
                        .cached_duplicate_fingerprint(
                            item.collection_id,
                            &source_identity,
                            DUPLICATE_FINGERPRINT_ALGORITHM_VERSION,
                        )
                        .transpose()
                })
                .transpose()?;
            if let Some(cached) = cached {
                self.repository
                    .complete_duplicate_scan_item(item.job_id, &cached, true)?;
                continue;
            }
            return Ok(Some(DuplicateFingerprintRequest {
                job_id: item.job_id,
                collection_id: item.collection_id,
                source_path: item.path,
            }));
        }
    }

    pub fn finish_duplicate_fingerprint(
        &mut self,
        request: &DuplicateFingerprintRequest,
        result: Result<SourceContentFingerprint, ThumbnailError>,
    ) -> ApplicationResult<DuplicateScanJobSnapshot> {
        match result {
            Ok(result) => {
                let fingerprint = DuplicateFingerprint {
                    collection_id: request.collection_id,
                    source_fingerprint: result.source_fingerprint,
                    algorithm_version: DUPLICATE_FINGERPRINT_ALGORITHM_VERSION.to_owned(),
                    source_size: result.source_size,
                    file_sha256: result.file_sha256,
                    archive_entry_count: result.archive_entry_count,
                    image_count: result.image_count,
                    content_fingerprint: result.content_fingerprint,
                    page_hashes: result.page_hashes,
                    perceptual_hashes: result.perceptual_hashes,
                    calculated_at: None,
                };
                Ok(self.repository.complete_duplicate_scan_item(
                    request.job_id,
                    &fingerprint,
                    false,
                )?)
            }
            Err(error) => Ok(self.repository.fail_duplicate_scan_item(
                request.job_id,
                request.collection_id,
                error.kind.as_str(),
                &error.message,
            )?),
        }
    }

    pub fn retry_duplicate_scan_failures(
        &mut self,
        job_id: i64,
    ) -> ApplicationResult<DuplicateScanJobSnapshot> {
        Ok(self.repository.retry_failed_duplicate_scan_items(job_id)?)
    }

    pub fn recover_interrupted_duplicate_scan_items(&mut self) -> ApplicationResult<usize> {
        Ok(self.repository.recover_interrupted_duplicate_scan_items()?)
    }

    pub fn duplicate_candidates(&self) -> ApplicationResult<Vec<DuplicateCandidatePair>> {
        Ok(self.repository.duplicate_candidates()?)
    }

    pub fn exclude_duplicate_pair(
        &mut self,
        left_collection_id: i64,
        left_identity: &str,
        right_collection_id: i64,
        right_identity: &str,
    ) -> ApplicationResult<()> {
        self.validate_current_duplicate_identities(
            left_collection_id,
            left_identity,
            right_collection_id,
            right_identity,
        )?;
        Ok(self.repository.exclude_duplicate_pair(
            left_collection_id,
            left_identity,
            right_collection_id,
            right_identity,
        )?)
    }

    pub fn confirm_duplicate_pair(
        &mut self,
        left_collection_id: i64,
        left_identity: &str,
        right_collection_id: i64,
        right_identity: &str,
    ) -> ApplicationResult<()> {
        self.validate_current_duplicate_identities(
            left_collection_id,
            left_identity,
            right_collection_id,
            right_identity,
        )?;
        Ok(self.repository.confirm_duplicate_pair(
            left_collection_id,
            left_identity,
            right_collection_id,
            right_identity,
        )?)
    }

    fn validate_current_duplicate_identities(
        &self,
        left_collection_id: i64,
        left_identity: &str,
        right_collection_id: i64,
        right_identity: &str,
    ) -> ApplicationResult<()> {
        for (collection_id, supplied) in [
            (left_collection_id, left_identity),
            (right_collection_id, right_identity),
        ] {
            let current = self
                .repository
                .duplicate_fingerprint(collection_id)?
                .ok_or_else(|| {
                    doujin_storage::StorageError::InvalidLifecycle(format!(
                        "收藏 {collection_id} 尚無 duplicate fingerprint"
                    ))
                })?;
            if current.identity() != supplied {
                return Err(doujin_storage::StorageError::InvalidLifecycle(format!(
                    "收藏 {collection_id} 的 fingerprint 已變更，請重新載入候選"
                ))
                .into());
            }
        }
        Ok(())
    }
}
