CREATE TABLE application_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    reader_path TEXT CHECK (reader_path IS NULL OR length(trim(reader_path)) > 0),
    thumbnail_width INTEGER NOT NULL CHECK (thumbnail_width BETWEEN 1 AND 4096),
    thumbnail_height INTEGER NOT NULL CHECK (thumbnail_height BETWEEN 1 AND 4096),
    thumbnail_quality INTEGER NOT NULL CHECK (thumbnail_quality BETWEEN 1 AND 100),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
