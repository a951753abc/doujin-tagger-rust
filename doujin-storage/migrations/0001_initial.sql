CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE library_roots (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL,
    path_key TEXT NOT NULL UNIQUE,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('archive', 'downloads')),
    label TEXT NOT NULL CHECK (length(trim(label)) > 0),
    active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE collections (
    id INTEGER PRIMARY KEY,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'tombstone', 'soft_deleted')),
    media_kind TEXT NOT NULL DEFAULT 'zip'
        CHECK (media_kind IN ('zip', 'image_folder')),
    parser_version TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE collection_locations (
    id INTEGER PRIMARY KEY,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    root_id INTEGER REFERENCES library_roots(id) ON DELETE RESTRICT,
    full_path TEXT NOT NULL,
    path_key TEXT NOT NULL,
    relative_path TEXT,
    filename TEXT NOT NULL CHECK (length(filename) > 0),
    location_status TEXT NOT NULL
        CHECK (location_status IN ('current', 'missing', 'moved', 'deleted')),
    discovered_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    ended_at TEXT,
    CHECK ((location_status = 'current' AND ended_at IS NULL)
        OR (location_status <> 'current' AND ended_at IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX collection_locations_current_path
    ON collection_locations(path_key) WHERE location_status = 'current';
CREATE UNIQUE INDEX collection_locations_current_collection
    ON collection_locations(collection_id) WHERE location_status = 'current';
CREATE INDEX collection_locations_collection
    ON collection_locations(collection_id, discovered_at);

CREATE TABLE parser_runs (
    id INTEGER PRIMARY KEY,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    parser_version TEXT NOT NULL,
    raw_filename TEXT NOT NULL,
    result_json TEXT NOT NULL CHECK (json_valid(result_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE metadata_assertions (
    id INTEGER PRIMARY KEY,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    field_name TEXT NOT NULL CHECK (field_name IN (
        'title', 'event', 'circle', 'authors', 'parody', 'classification', 'is_dl'
    )),
    value_json TEXT NOT NULL CHECK (json_valid(value_json)),
    source_kind TEXT NOT NULL
        CHECK (source_kind IN ('manual', 'legacy', 'external', 'filename', 'inference')),
    parser_run_id INTEGER REFERENCES parser_runs(id) ON DELETE RESTRICT,
    source_reference TEXT,
    confidence_total REAL CHECK (
        confidence_total IS NULL OR (confidence_total >= 0.0 AND confidence_total <= 1.0)
    ),
    confidence_json TEXT CHECK (confidence_json IS NULL OR json_valid(confidence_json)),
    status TEXT NOT NULL DEFAULT 'candidate'
        CHECK (status IN ('candidate', 'accepted', 'rejected', 'obsolete')),
    reason TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (id, collection_id, field_name),
    CHECK (source_kind = 'filename' OR parser_run_id IS NULL),
    CHECK (source_kind <> 'filename' OR parser_run_id IS NOT NULL)
) STRICT;

CREATE INDEX metadata_assertions_collection_field
    ON metadata_assertions(collection_id, field_name, status);

CREATE TABLE metadata_selections (
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    field_name TEXT NOT NULL,
    assertion_id INTEGER NOT NULL,
    selected_by TEXT NOT NULL CHECK (selected_by IN ('priority', 'manual', 'migration')),
    selected_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (collection_id, field_name),
    FOREIGN KEY (assertion_id, collection_id, field_name)
        REFERENCES metadata_assertions(id, collection_id, field_name) ON DELETE RESTRICT
) STRICT;

CREATE TABLE canonical_entities (
    id INTEGER PRIMARY KEY,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('event', 'circle', 'author', 'parody')),
    canonical_name TEXT NOT NULL CHECK (length(trim(canonical_name)) > 0),
    is_official INTEGER NOT NULL DEFAULT 0 CHECK (is_official IN (0, 1)),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'merged', 'retired')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (entity_kind, canonical_name)
) STRICT;

CREATE TABLE name_variants (
    id INTEGER PRIMARY KEY,
    entity_id INTEGER NOT NULL REFERENCES canonical_entities(id) ON DELETE RESTRICT,
    raw_name TEXT NOT NULL CHECK (length(raw_name) > 0),
    source_kind TEXT NOT NULL
        CHECK (source_kind IN ('manual', 'legacy', 'external', 'filename', 'inference')),
    evidence_json TEXT CHECK (evidence_json IS NULL OR json_valid(evidence_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (entity_id, raw_name)
) STRICT;

CREATE TABLE assertion_entities (
    assertion_id INTEGER NOT NULL REFERENCES metadata_assertions(id) ON DELETE CASCADE,
    entity_id INTEGER NOT NULL REFERENCES canonical_entities(id) ON DELETE RESTRICT,
    value_index INTEGER NOT NULL CHECK (value_index >= 0),
    raw_name TEXT NOT NULL,
    evidence_json TEXT CHECK (evidence_json IS NULL OR json_valid(evidence_json)),
    PRIMARY KEY (assertion_id, value_index)
) STRICT;

CREATE INDEX assertion_entities_entity
    ON assertion_entities(entity_id, assertion_id);

CREATE TABLE merge_exclusions (
    left_entity_id INTEGER NOT NULL REFERENCES canonical_entities(id) ON DELETE RESTRICT,
    right_entity_id INTEGER NOT NULL REFERENCES canonical_entities(id) ON DELETE RESTRICT,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (left_entity_id, right_entity_id),
    CHECK (left_entity_id < right_entity_id)
) STRICT;

CREATE TABLE tags (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE collection_tags (
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (collection_id, tag_id)
) STRICT;

CREATE TABLE effective_metadata (
    collection_id INTEGER PRIMARY KEY REFERENCES collections(id) ON DELETE CASCADE,
    title TEXT CHECK (title IS NULL OR length(trim(title)) > 0),
    event TEXT,
    circle TEXT,
    authors TEXT NOT NULL DEFAULT '',
    authors_json TEXT NOT NULL CHECK (json_valid(authors_json)),
    parody TEXT,
    parody_raw TEXT,
    classification_top TEXT CHECK (
        classification_top IS NULL OR length(trim(classification_top)) > 0
    ),
    classification_subcategory TEXT,
    is_dl INTEGER CHECK (is_dl IS NULL OR is_dl IN (0, 1)),
    projection_version INTEGER NOT NULL DEFAULT 1 CHECK (projection_version > 0),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE external_search_results (
    id INTEGER PRIMARY KEY,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    field_name TEXT NOT NULL CHECK (field_name IN (
        'title', 'event', 'circle', 'authors', 'parody', 'classification', 'is_dl'
    )),
    value_json TEXT NOT NULL CHECK (json_valid(value_json)),
    source_reference TEXT NOT NULL CHECK (length(trim(source_reference)) > 0),
    confidence_total REAL NOT NULL CHECK (confidence_total >= 0.0 AND confidence_total <= 1.0),
    confidence_json TEXT NOT NULL CHECK (json_valid(confidence_json)),
    disposition TEXT NOT NULL
        CHECK (disposition IN ('search_only', 'suggestion', 'auto_applied')),
    assertion_id INTEGER REFERENCES metadata_assertions(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK ((disposition = 'search_only' AND assertion_id IS NULL)
        OR (disposition <> 'search_only' AND assertion_id IS NOT NULL))
) STRICT;

CREATE INDEX external_search_results_collection_field
    ON external_search_results(collection_id, field_name, created_at);

CREATE TABLE tombstone_candidates (
    tombstone_collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    candidate_collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    reason TEXT NOT NULL,
    decision TEXT NOT NULL DEFAULT 'pending'
        CHECK (decision IN ('pending', 'confirmed', 'rejected')),
    discovered_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    decided_at TEXT,
    PRIMARY KEY (tombstone_collection_id, candidate_collection_id),
    CHECK (tombstone_collection_id <> candidate_collection_id),
    CHECK ((decision = 'pending' AND decided_at IS NULL)
        OR (decision <> 'pending' AND decided_at IS NOT NULL))
) STRICT;

CREATE TABLE file_operations (
    id INTEGER PRIMARY KEY,
    collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL,
    from_location_id INTEGER REFERENCES collection_locations(id) ON DELETE SET NULL,
    to_root_id INTEGER REFERENCES library_roots(id) ON DELETE RESTRICT,
    operation_kind TEXT NOT NULL
        CHECK (operation_kind IN ('rename', 'move', 'soft_delete', 'hard_delete')),
    from_path TEXT NOT NULL,
    to_path TEXT,
    status TEXT NOT NULL CHECK (status IN ('pending', 'succeeded', 'failed')),
    error_message TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at TEXT,
    CHECK ((status = 'pending' AND completed_at IS NULL)
        OR (status <> 'pending' AND completed_at IS NOT NULL)),
    CHECK (status <> 'failed' OR error_message IS NOT NULL)
) STRICT;

CREATE TABLE scan_runs (
    id INTEGER PRIMARY KEY,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at TEXT,
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'partial', 'failed')),
    summary_json TEXT CHECK (summary_json IS NULL OR json_valid(summary_json)),
    error_message TEXT
) STRICT;

CREATE TABLE scan_issues (
    id INTEGER PRIMARY KEY,
    scan_run_id INTEGER NOT NULL REFERENCES scan_runs(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    issue_kind TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE background_jobs (
    id INTEGER PRIMARY KEY,
    collection_id INTEGER REFERENCES collections(id) ON DELETE CASCADE,
    job_kind TEXT NOT NULL CHECK (job_kind IN ('external_search', 'thumbnail')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    error_kind TEXT,
    error_message TEXT,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_retry_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE VIRTUAL TABLE collection_fts USING fts5(
    title,
    circle,
    authors,
    parody,
    content = 'effective_metadata',
    content_rowid = 'collection_id'
);

CREATE TRIGGER effective_metadata_after_insert AFTER INSERT ON effective_metadata BEGIN
    INSERT INTO collection_fts(rowid, title, circle, authors, parody)
    VALUES (new.collection_id, new.title, new.circle, new.authors, new.parody);
END;

CREATE TRIGGER effective_metadata_after_delete AFTER DELETE ON effective_metadata BEGIN
    INSERT INTO collection_fts(collection_fts, rowid, title, circle, authors, parody)
    VALUES ('delete', old.collection_id, old.title, old.circle, old.authors, old.parody);
END;

CREATE TRIGGER effective_metadata_after_update AFTER UPDATE ON effective_metadata BEGIN
    INSERT INTO collection_fts(collection_fts, rowid, title, circle, authors, parody)
    VALUES ('delete', old.collection_id, old.title, old.circle, old.authors, old.parody);
    INSERT INTO collection_fts(rowid, title, circle, authors, parody)
    VALUES (new.collection_id, new.title, new.circle, new.authors, new.parody);
END;
