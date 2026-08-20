//! Encrypted persistence for the ExHentai browser session Cookie.

use rusqlite::OptionalExtension;
use windows_dpapi::{Scope, decrypt_data, encrypt_data};

use crate::{CatalogRepository, StorageError, StorageResult};

const PROTECTION_VERSION: i64 = 1;
const PROTECTION_SCOPE: &str = "user";
const APPLICATION_ENTROPY: &[u8] = b"doujin-tagger-rust/exhentai-cookie/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExHentaiSessionStatus {
    pub configured: bool,
    pub updated_at: Option<String>,
}

impl CatalogRepository {
    /// Reads and decrypts the saved Cookie for the current Windows user.
    ///
    /// A protected value copied from another machine/account, or a damaged
    /// value, returns [`StorageError::ExHentaiCookieUnavailable`] so callers
    /// can request that the user configure it again.
    pub fn exhentai_cookie(&self) -> StorageResult<Option<String>> {
        let protected = self
            .connection
            .query_row(
                "SELECT encrypted_cookie, protection_version, protection_scope
                 FROM exhentai_session WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;

        let Some((encrypted_cookie, protection_version, protection_scope)) = protected else {
            return Ok(None);
        };
        if protection_version != PROTECTION_VERSION || protection_scope != PROTECTION_SCOPE {
            return Err(StorageError::ExHentaiCookieUnavailable);
        }

        let cookie = decrypt_data(&encrypted_cookie, Scope::User, Some(APPLICATION_ENTROPY))
            .map_err(|_| StorageError::ExHentaiCookieUnavailable)?;
        String::from_utf8(cookie)
            .map(Some)
            .map_err(|_| StorageError::ExHentaiCookieUnavailable)
    }

    /// Encrypts and saves a non-blank Cookie for the current Windows user.
    pub fn save_exhentai_cookie(&mut self, cookie: &str) -> StorageResult<ExHentaiSessionStatus> {
        if cookie.trim().is_empty() {
            return Err(StorageError::InvalidExHentaiCookie);
        }
        let encrypted_cookie =
            encrypt_data(cookie.as_bytes(), Scope::User, Some(APPLICATION_ENTROPY))
                .map_err(|_| StorageError::ExHentaiCookieProtectionFailed)?;
        self.connection.execute(
            "INSERT INTO exhentai_session(
                 singleton, encrypted_cookie, protection_version, protection_scope
             ) VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(singleton) DO UPDATE SET
                 encrypted_cookie = excluded.encrypted_cookie,
                 protection_version = excluded.protection_version,
                 protection_scope = excluded.protection_scope,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            (&encrypted_cookie, PROTECTION_VERSION, PROTECTION_SCOPE),
        )?;
        self.exhentai_session_status()
    }

    /// Removes the saved Cookie. Returns whether a configured row existed.
    pub fn clear_exhentai_cookie(&mut self) -> StorageResult<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM exhentai_session WHERE singleton = 1", [])?
            > 0)
    }

    /// Reports configuration metadata without reading or decrypting the Cookie.
    pub fn exhentai_session_status(&self) -> StorageResult<ExHentaiSessionStatus> {
        let updated_at = self
            .connection
            .query_row(
                "SELECT updated_at FROM exhentai_session WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(ExHentaiSessionStatus {
            configured: updated_at.is_some(),
            updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use rusqlite::Connection;

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestCatalog {
        directory: PathBuf,
        database: PathBuf,
    }

    impl TestCatalog {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "doujin-storage-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&directory).expect("create test directory");
            let database = directory.join("catalog.db");
            Self {
                directory,
                database,
            }
        }

        fn path(&self) -> &Path {
            &self.database
        }
    }

    impl Drop for TestCatalog {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn migration_21_upgrades_a_version_20_catalog() {
        let catalog = TestCatalog::new("exhentai-migration");
        drop(CatalogRepository::open(catalog.path()).expect("create current catalog"));
        let connection = Connection::open(catalog.path()).expect("open raw catalog");
        connection
            .execute_batch(
                "DROP TABLE exhentai_session;
                 DELETE FROM schema_migrations WHERE version = 21;
                 PRAGMA user_version = 20;",
            )
            .expect("rewind catalog to version 20");
        drop(connection);

        let repository = CatalogRepository::open(catalog.path()).expect("upgrade catalog");
        assert_eq!(21, repository.schema_version().expect("schema version"));
        assert!(
            repository
                .table_is_strict("exhentai_session")
                .expect("strict session table")
        );
        assert_eq!(
            ExHentaiSessionStatus {
                configured: false,
                updated_at: None,
            },
            repository
                .exhentai_session_status()
                .expect("empty session status")
        );
    }

    #[test]
    fn cookie_is_encrypted_reopened_updated_and_cleared() {
        let catalog = TestCatalog::new("exhentai-round-trip");
        let original = "ipb_member_id=123; ipb_pass_hash=raw-secret";
        let replacement = "ipb_member_id=456; ipb_pass_hash=new-secret";
        let first_ciphertext = {
            let mut repository = CatalogRepository::open(catalog.path()).expect("open catalog");
            let status = repository
                .save_exhentai_cookie(original)
                .expect("save Cookie");
            assert!(status.configured);
            assert!(status.updated_at.is_some());
            let ciphertext: Vec<u8> = repository
                .connection
                .query_row(
                    "SELECT encrypted_cookie FROM exhentai_session WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .expect("read ciphertext");
            assert_ne!(original.as_bytes(), ciphertext);
            assert!(
                !ciphertext
                    .windows(original.len())
                    .any(|bytes| bytes == original.as_bytes())
            );
            ciphertext
        };

        let mut repository = CatalogRepository::open(catalog.path()).expect("reopen catalog");
        assert_eq!(
            Some(original.to_owned()),
            repository.exhentai_cookie().expect("decrypt Cookie")
        );
        repository
            .save_exhentai_cookie(replacement)
            .expect("update Cookie");
        let second_ciphertext: Vec<u8> = repository
            .connection
            .query_row(
                "SELECT encrypted_cookie FROM exhentai_session WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .expect("read updated ciphertext");
        assert_ne!(first_ciphertext, second_ciphertext);
        assert!(
            !second_ciphertext
                .windows(replacement.len())
                .any(|bytes| bytes == replacement.as_bytes())
        );
        assert!(repository.clear_exhentai_cookie().expect("clear Cookie"));
        assert!(
            !repository
                .clear_exhentai_cookie()
                .expect("clear absent Cookie")
        );
        assert_eq!(None, repository.exhentai_cookie().expect("empty Cookie"));
        assert!(
            !repository
                .exhentai_session_status()
                .expect("cleared status")
                .configured
        );
    }

    #[test]
    fn blank_cookie_is_rejected_without_changing_the_saved_value() {
        let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
        repository
            .save_exhentai_cookie("valid-cookie")
            .expect("save initial Cookie");
        assert!(matches!(
            repository.save_exhentai_cookie(" \t\r\n"),
            Err(StorageError::InvalidExHentaiCookie)
        ));
        assert_eq!(
            Some("valid-cookie".to_owned()),
            repository.exhentai_cookie().expect("unchanged Cookie")
        );
    }

    #[test]
    fn damaged_ciphertext_returns_a_safe_reconfiguration_error() {
        let secret = "ipb_member_id=123; ipb_pass_hash=must-not-leak";
        let mut repository = CatalogRepository::open_in_memory().expect("open catalog");
        repository
            .save_exhentai_cookie(secret)
            .expect("save Cookie");
        repository
            .connection
            .execute(
                "UPDATE exhentai_session SET encrypted_cookie = X'010203' WHERE singleton = 1",
                [],
            )
            .expect("damage ciphertext");

        let error = repository
            .exhentai_cookie()
            .expect_err("damaged ciphertext must fail");
        assert!(matches!(error, StorageError::ExHentaiCookieUnavailable));
        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));
        assert!(
            repository
                .exhentai_session_status()
                .expect("status does not decrypt")
                .configured
        );
    }
}
