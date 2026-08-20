CREATE TABLE exhentai_session (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    encrypted_cookie BLOB NOT NULL CHECK(length(encrypted_cookie) > 0),
    protection_version INTEGER NOT NULL CHECK(protection_version = 1),
    protection_scope TEXT NOT NULL CHECK(protection_scope = 'user'),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
