CREATE TABLE saved_views (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL COLLATE NOCASE
        CHECK(length(trim(name)) BETWEEN 1 AND 80),
    query_json TEXT NOT NULL CHECK(json_valid(query_json)),
    layout TEXT NOT NULL DEFAULT 'grid'
        CHECK(layout IN ('grid', 'list')),
    pinned INTEGER NOT NULL DEFAULT 1
        CHECK(pinned IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

CREATE UNIQUE INDEX saved_views_name_unique
ON saved_views(name COLLATE NOCASE);

CREATE INDEX saved_views_navigation_order
ON saved_views(pinned DESC, updated_at DESC, id DESC);
