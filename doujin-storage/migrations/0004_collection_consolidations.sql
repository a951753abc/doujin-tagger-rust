CREATE TABLE collection_consolidations (
    id INTEGER PRIMARY KEY,
    survivor_collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE RESTRICT,
    merged_collection_id INTEGER NOT NULL UNIQUE REFERENCES collections(id) ON DELETE RESTRICT,
    resolutions_json TEXT NOT NULL CHECK (json_valid(resolutions_json)),
    consolidated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (survivor_collection_id <> merged_collection_id)
) STRICT;

CREATE INDEX collection_consolidations_survivor
    ON collection_consolidations(survivor_collection_id, consolidated_at);

CREATE TABLE collection_consolidation_transfers (
    consolidation_id INTEGER NOT NULL
        REFERENCES collection_consolidations(id) ON DELETE CASCADE,
    record_kind TEXT NOT NULL CHECK (record_kind IN (
        'location', 'parser_run', 'metadata_assertion', 'external_search_result',
        'tag_relation', 'file_operation'
    )),
    record_id INTEGER NOT NULL,
    original_collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE RESTRICT,
    PRIMARY KEY (consolidation_id, record_kind, record_id)
) STRICT;
