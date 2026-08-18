CREATE TABLE external_search_job_resolutions (
    job_id INTEGER PRIMARY KEY REFERENCES background_jobs(id) ON DELETE CASCADE,
    acknowledged_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
