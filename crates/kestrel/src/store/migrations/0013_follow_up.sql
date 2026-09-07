ALTER TABLE session ADD COLUMN turn_pending INTEGER NOT NULL DEFAULT FALSE;

ALTER TABLE integration ADD COLUMN comments_polled_through INTEGER;

ALTER TABLE event ADD COLUMN message TEXT;

CREATE TABLE follow_up (
    event_id TEXT PRIMARY KEY REFERENCES event (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    session_id TEXT REFERENCES session (id),
    received_at TEXT NOT NULL
) STRICT;
