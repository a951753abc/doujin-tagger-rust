CREATE TABLE thumbnail_states (
    collection_id INTEGER PRIMARY KEY REFERENCES collections(id) ON DELETE CASCADE,
    source_fingerprint TEXT NOT NULL,
    settings_fingerprint TEXT NOT NULL,
    cache_path TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'ready', 'failed')),
    error_kind TEXT CHECK (error_kind IS NULL OR error_kind IN (
        'source_io', 'cache_io', 'worker_interrupted',
        'invalid_archive', 'no_supported_image', 'image_decode',
        'resource_limit', 'unsupported'
    )),
    error_message TEXT,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_retry_at TEXT,
    failed_at TEXT,
    generated_width INTEGER CHECK (generated_width IS NULL OR generated_width > 0),
    generated_height INTEGER CHECK (generated_height IS NULL OR generated_height > 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (next_retry_at IS NULL OR status = 'pending'),
    CHECK (status <> 'ready' OR (generated_width IS NOT NULL AND generated_height IS NOT NULL))
) STRICT;

CREATE INDEX thumbnail_states_due
    ON thumbnail_states(status, next_retry_at, updated_at)
    WHERE status = 'pending';
