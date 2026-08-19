CREATE TABLE shelf_configuration (
    position INTEGER PRIMARY KEY CHECK(position >= 0),
    shelf_type TEXT NOT NULL CHECK(shelf_type IN ('recent', 'featured', 'event', 'saved_view')),
    saved_view_id INTEGER REFERENCES saved_views(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    preview_limit INTEGER NOT NULL CHECK(preview_limit IN (6, 8, 12, 16)),
    CHECK(
        (shelf_type = 'saved_view' AND saved_view_id IS NOT NULL)
        OR (shelf_type <> 'saved_view' AND saved_view_id IS NULL)
    )
) STRICT;

CREATE UNIQUE INDEX shelf_configuration_saved_view_unique
ON shelf_configuration(saved_view_id)
WHERE saved_view_id IS NOT NULL;

INSERT INTO shelf_configuration(position, shelf_type, saved_view_id, enabled, preview_limit)
VALUES
    (0, 'recent', NULL, 1, 8),
    (1, 'featured', NULL, 1, 8),
    (2, 'event', NULL, 1, 8);

INSERT INTO shelf_configuration(position, shelf_type, saved_view_id, enabled, preview_limit)
SELECT
    2 + ROW_NUMBER() OVER (ORDER BY updated_at DESC, id DESC),
    'saved_view',
    id,
    1,
    8
FROM saved_views
WHERE pinned = 1;
