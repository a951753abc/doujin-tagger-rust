INSERT INTO metadata_assertions(
    collection_id,
    field_name,
    value_json,
    source_kind,
    source_reference,
    confidence_total,
    status,
    reason
)
SELECT
    metadata.collection_id,
    'event',
    json_quote('DL'),
    'inference',
    'rule:dl-without-event',
    1.0,
    'accepted',
    'dl_without_event_fallback'
FROM effective_metadata AS metadata
WHERE metadata.is_dl = 1
  AND NULLIF(trim(metadata.event), '') IS NULL
  AND NOT EXISTS (
      SELECT 1
      FROM metadata_assertions AS assertion
      WHERE assertion.collection_id = metadata.collection_id
        AND assertion.field_name = 'event'
        AND assertion.source_kind = 'inference'
        AND assertion.source_reference = 'rule:dl-without-event'
  );

INSERT INTO metadata_selections(collection_id, field_name, assertion_id, selected_by)
SELECT
    assertion.collection_id,
    'event',
    assertion.id,
    'priority'
FROM metadata_assertions AS assertion
JOIN effective_metadata AS metadata
  ON metadata.collection_id = assertion.collection_id
WHERE assertion.field_name = 'event'
  AND assertion.source_kind = 'inference'
  AND assertion.source_reference = 'rule:dl-without-event'
  AND assertion.status = 'accepted'
  AND metadata.is_dl = 1
  AND NULLIF(trim(metadata.event), '') IS NULL
ON CONFLICT(collection_id, field_name) DO NOTHING;

UPDATE effective_metadata
SET event = 'DL',
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE is_dl = 1
  AND NULLIF(trim(event), '') IS NULL
  AND EXISTS (
      SELECT 1
      FROM metadata_selections AS selection
      JOIN metadata_assertions AS assertion
        ON assertion.id = selection.assertion_id
      WHERE selection.collection_id = effective_metadata.collection_id
        AND selection.field_name = 'event'
        AND assertion.source_kind = 'inference'
        AND assertion.source_reference = 'rule:dl-without-event'
  );
