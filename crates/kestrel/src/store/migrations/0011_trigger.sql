CREATE TABLE trigger (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    name TEXT NOT NULL,
    filter TEXT NOT NULL,
    brief TEXT NOT NULL,
    branch TEXT,
    correlation TEXT,
    workspace_id TEXT NOT NULL REFERENCES workspace (id),
    agent_id TEXT NOT NULL REFERENCES agent (id),
    state TEXT NOT NULL,
    declared_at TEXT NOT NULL,
    UNIQUE (organization_id, name)
) STRICT;

-- One Trigger matching one Event, and either the Session that match opened or why it opened
-- none. The key is the whole of why relabelling an issue twice opens one Session, and why a
-- firing that failed is not retried by every sweep after it.
CREATE TABLE firing (
    trigger_id TEXT NOT NULL REFERENCES trigger (id),
    event_record_id TEXT NOT NULL REFERENCES event (record_id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    session_id TEXT REFERENCES session (id),
    failure TEXT,
    fired_at TEXT NOT NULL,
    PRIMARY KEY (trigger_id, event_record_id),
    CHECK ((session_id IS NULL) <> (failure IS NULL))
) STRICT;

ALTER TABLE session ADD COLUMN event_record_id TEXT REFERENCES event (record_id);
ALTER TABLE session ADD COLUMN correlation TEXT;

CREATE UNIQUE INDEX session_open_correlation ON session (organization_id, correlation)
    WHERE state = 'open' AND correlation IS NOT NULL;
