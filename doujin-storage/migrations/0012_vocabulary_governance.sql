CREATE TABLE vocabulary_aliases (
    field_name TEXT NOT NULL CHECK (field_name IN ('event', 'circle', 'author', 'parody')),
    alias TEXT NOT NULL CHECK (length(alias) > 0),
    entity_id INTEGER NOT NULL REFERENCES canonical_entities(id) ON DELETE RESTRICT,
    source TEXT NOT NULL CHECK (source IN ('user_confirmed')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (field_name, alias)
) STRICT;

CREATE INDEX vocabulary_aliases_entity
    ON vocabulary_aliases(entity_id, field_name);

CREATE TABLE vocabulary_exclusions (
    field_name TEXT NOT NULL CHECK (field_name IN ('event', 'circle', 'author', 'parody')),
    left_value TEXT NOT NULL CHECK (length(left_value) > 0),
    right_value TEXT NOT NULL CHECK (length(right_value) > 0),
    reason TEXT NOT NULL CHECK (length(trim(reason)) > 0),
    source TEXT NOT NULL CHECK (source IN ('user_rejected', 'user_removed')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (field_name, left_value, right_value),
    CHECK (left_value < right_value)
) STRICT;
