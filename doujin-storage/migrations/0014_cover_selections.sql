CREATE TABLE cover_selections (
    collection_id INTEGER PRIMARY KEY REFERENCES collections(id) ON DELETE CASCADE,
    entry_path TEXT NOT NULL CHECK (length(trim(entry_path)) > 0),
    source_fingerprint TEXT NOT NULL CHECK (length(trim(source_fingerprint)) > 0),
    validation_status TEXT NOT NULL DEFAULT 'valid'
        CHECK (validation_status IN ('valid', 'source_changed', 'missing')),
    validation_error TEXT,
    selected_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
