ALTER TABLE thumbnail_states
ADD COLUMN priority INTEGER NOT NULL DEFAULT 0
CHECK(priority BETWEEN 0 AND 9007199254740991);

ALTER TABLE thumbnail_states
ADD COLUMN requested_at TEXT;

UPDATE thumbnail_states
SET requested_at = updated_at;

CREATE INDEX thumbnail_states_schedule_idx
ON thumbnail_states(status, priority DESC, requested_at ASC, next_retry_at, collection_id DESC);
