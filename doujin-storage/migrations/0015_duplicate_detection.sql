CREATE TABLE duplicate_fingerprints (
    collection_id INTEGER PRIMARY KEY REFERENCES collections(id) ON DELETE CASCADE,
    source_fingerprint TEXT NOT NULL CHECK (length(trim(source_fingerprint)) > 0),
    algorithm_version TEXT NOT NULL CHECK (length(trim(algorithm_version)) > 0),
    source_size INTEGER NOT NULL CHECK (source_size >= 0),
    file_sha256 TEXT CHECK (file_sha256 IS NULL OR length(file_sha256) = 64),
    archive_entry_count INTEGER NOT NULL CHECK (archive_entry_count >= 0),
    image_count INTEGER NOT NULL CHECK (image_count >= 0),
    content_fingerprint TEXT NOT NULL CHECK (length(content_fingerprint) = 64),
    page_hashes_json TEXT NOT NULL CHECK (json_valid(page_hashes_json)),
    perceptual_hashes_json TEXT CHECK (
        perceptual_hashes_json IS NULL OR json_valid(perceptual_hashes_json)
    ),
    calculated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE INDEX duplicate_fingerprints_file_sha
    ON duplicate_fingerprints(file_sha256) WHERE file_sha256 IS NOT NULL;
CREATE INDEX duplicate_fingerprints_content
    ON duplicate_fingerprints(content_fingerprint, image_count);

CREATE TABLE duplicate_scan_jobs (
    id INTEGER PRIMARY KEY,
    status TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'completed', 'completed_with_errors')),
    total INTEGER NOT NULL CHECK (total >= 0),
    concurrency_limit INTEGER NOT NULL CHECK (concurrency_limit BETWEEN 1 AND 8),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at TEXT
) STRICT;

CREATE UNIQUE INDEX duplicate_scan_jobs_one_running
    ON duplicate_scan_jobs(status) WHERE status = 'running';

CREATE TABLE duplicate_scan_items (
    job_id INTEGER NOT NULL REFERENCES duplicate_scan_jobs(id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'processed', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    reused_cache INTEGER NOT NULL DEFAULT 0 CHECK (reused_cache IN (0, 1)),
    error_kind TEXT,
    error_message TEXT,
    started_at TEXT,
    completed_at TEXT,
    PRIMARY KEY (job_id, collection_id),
    CHECK ((status = 'failed' AND error_kind IS NOT NULL AND error_message IS NOT NULL)
        OR (status <> 'failed' AND error_kind IS NULL AND error_message IS NULL))
) STRICT;

CREATE INDEX duplicate_scan_items_queue
    ON duplicate_scan_items(status, job_id, collection_id);

CREATE TABLE duplicate_exclusions (
    left_collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    right_collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    left_fingerprint_identity TEXT NOT NULL,
    right_fingerprint_identity TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT 'not_duplicate',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (
        left_collection_id,
        right_collection_id,
        left_fingerprint_identity,
        right_fingerprint_identity
    ),
    CHECK (left_collection_id < right_collection_id)
) STRICT;

CREATE TABLE duplicate_reviews (
    left_collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    right_collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    left_fingerprint_identity TEXT NOT NULL,
    right_fingerprint_identity TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision = 'confirmed_duplicate'),
    reviewed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (
        left_collection_id,
        right_collection_id,
        left_fingerprint_identity,
        right_fingerprint_identity
    ),
    CHECK (left_collection_id < right_collection_id)
) STRICT;
