ALTER TABLE integration ADD COLUMN comments_polled_through INTEGER;

ALTER TABLE event ADD COLUMN message TEXT;

ALTER TABLE run ADD COLUMN supervisor_state TEXT NOT NULL DEFAULT 'absent'
    CHECK (supervisor_state IN ('absent', 'present', 'gone'));
ALTER TABLE run ADD COLUMN supervisor TEXT;

CREATE TABLE pending_message (
    session_id TEXT NOT NULL REFERENCES session (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    seq INTEGER NOT NULL,
    participant TEXT NOT NULL,
    body TEXT NOT NULL,
    received_at TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
) STRICT;

CREATE TABLE follow_up (
    event_record_id TEXT PRIMARY KEY REFERENCES event (record_id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    session_id TEXT NOT NULL REFERENCES session (id),
    received_at TEXT NOT NULL
) STRICT;
