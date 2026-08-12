DELETE FROM metadata_selections
WHERE field_name = 'event'
  AND assertion_id IN (
      SELECT id
      FROM metadata_assertions
      WHERE field_name = 'event'
        AND source_kind = 'inference'
        AND source_reference = 'rule:dl-without-event'
        AND reason = 'dl_without_event_fallback'
  );

UPDATE effective_metadata
SET event = NULL,
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE EXISTS (
    SELECT 1
    FROM metadata_assertions AS assertion
    WHERE assertion.collection_id = effective_metadata.collection_id
      AND assertion.field_name = 'event'
      AND assertion.source_kind = 'inference'
      AND assertion.source_reference = 'rule:dl-without-event'
      AND assertion.reason = 'dl_without_event_fallback'
)
AND NOT EXISTS (
    SELECT 1
    FROM metadata_selections AS selection
    WHERE selection.collection_id = effective_metadata.collection_id
      AND selection.field_name = 'event'
);

DELETE FROM metadata_assertions
WHERE field_name = 'event'
  AND source_kind = 'inference'
  AND source_reference = 'rule:dl-without-event'
  AND reason = 'dl_without_event_fallback';
