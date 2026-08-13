CREATE TABLE external_search_batches (
    id INTEGER PRIMARY KEY,
    strategy TEXT NOT NULL CHECK (strategy IN ('only_missing', 'specified')),
    fields_json TEXT NOT NULL CHECK (json_valid(fields_json) AND json_type(fields_json) = 'array'),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE external_search_batch_items (
    id INTEGER PRIMARY KEY,
    batch_id INTEGER NOT NULL REFERENCES external_search_batches(id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL,
    job_id INTEGER REFERENCES background_jobs(id) ON DELETE SET NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('enqueued', 'reused', 'skipped', 'unchanged')),
    fields_json TEXT NOT NULL CHECK (json_valid(fields_json) AND json_type(fields_json) = 'array'),
    reason TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(batch_id, collection_id),
    CHECK ((outcome IN ('enqueued', 'reused') AND job_id IS NOT NULL)
        OR (outcome IN ('skipped', 'unchanged') AND job_id IS NULL))
) STRICT;

CREATE INDEX external_search_batch_items_job
    ON external_search_batch_items(job_id) WHERE job_id IS NOT NULL;
