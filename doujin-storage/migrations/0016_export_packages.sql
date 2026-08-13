CREATE TABLE export_roots (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL,
    path_key TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL CHECK (length(trim(label)) > 0),
    active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE export_jobs (
    id INTEGER PRIMARY KEY,
    export_root_id INTEGER NOT NULL REFERENCES export_roots(id) ON DELETE RESTRICT,
    package_filename TEXT NOT NULL CHECK (length(trim(package_filename)) > 0),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
    total_items INTEGER NOT NULL CHECK (total_items > 0),
    processed_items INTEGER NOT NULL DEFAULT 0 CHECK (processed_items >= 0),
    total_bytes INTEGER NOT NULL CHECK (total_bytes >= 0),
    processed_bytes INTEGER NOT NULL DEFAULT 0 CHECK (processed_bytes >= 0),
    current_collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL,
    succeeded_items INTEGER NOT NULL DEFAULT 0 CHECK (succeeded_items >= 0),
    failed_items INTEGER NOT NULL DEFAULT 0 CHECK (failed_items >= 0),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    error_message TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at TEXT,
    CHECK (processed_items <= total_items),
    CHECK (processed_bytes <= total_bytes),
    CHECK ((status IN ('pending', 'running') AND completed_at IS NULL)
        OR (status IN ('succeeded', 'failed') AND completed_at IS NOT NULL)),
    CHECK (status <> 'failed' OR error_message IS NOT NULL)
) STRICT;

CREATE INDEX export_jobs_recent ON export_jobs(created_at DESC, id DESC);
CREATE UNIQUE INDEX export_jobs_one_running
    ON export_jobs(status) WHERE status = 'running';

CREATE TABLE export_job_items (
    job_id INTEGER NOT NULL REFERENCES export_jobs(id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE RESTRICT,
    package_entry TEXT NOT NULL CHECK (length(trim(package_entry)) > 0),
    original_filename TEXT NOT NULL CHECK (length(trim(original_filename)) > 0),
    expected_source_identity TEXT NOT NULL CHECK (length(expected_source_identity) > 0),
    source_size INTEGER NOT NULL CHECK (source_size >= 0),
    manifest_json TEXT NOT NULL CHECK (json_valid(manifest_json)),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
    bytes_copied INTEGER NOT NULL DEFAULT 0 CHECK (bytes_copied >= 0),
    error_message TEXT,
    started_at TEXT,
    completed_at TEXT,
    PRIMARY KEY (job_id, collection_id),
    UNIQUE (job_id, package_entry),
    CHECK (bytes_copied <= source_size),
    CHECK ((status = 'failed' AND error_message IS NOT NULL)
        OR (status <> 'failed' AND error_message IS NULL))
) STRICT;

CREATE INDEX export_job_items_status
    ON export_job_items(job_id, status, collection_id);
