ALTER TABLE background_jobs RENAME TO background_jobs_v2;

CREATE TABLE background_jobs (
    id INTEGER PRIMARY KEY,
    collection_id INTEGER REFERENCES collections(id) ON DELETE CASCADE,
    job_kind TEXT NOT NULL CHECK (job_kind IN ('external_search', 'thumbnail')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'partial', 'failed')),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
    error_kind TEXT,
    error_message TEXT,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_retry_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (next_retry_at IS NULL OR status = 'pending')
) STRICT;

INSERT INTO background_jobs(
    id, collection_id, job_kind, status, payload_json, error_kind, error_message,
    attempts, next_retry_at, created_at, updated_at
)
SELECT
    id, collection_id, job_kind, status, payload_json, error_kind, error_message,
    attempts, next_retry_at, created_at, updated_at
FROM background_jobs_v2;

DROP TABLE background_jobs_v2;

CREATE UNIQUE INDEX background_jobs_one_active_external_search
    ON background_jobs(collection_id)
    WHERE job_kind = 'external_search'
      AND status IN ('pending', 'running')
      AND collection_id IS NOT NULL;
