CREATE TABLE trigger (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    name TEXT NOT NULL,
    repository TEXT NOT NULL,
    label TEXT NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspace (id),
    agent_id TEXT NOT NULL REFERENCES agent (id),
    state TEXT NOT NULL,
    declared_at TEXT NOT NULL,
    UNIQUE (organization_id, name)
) STRICT;

-- One Trigger matching one Event, and the Session that match opened. The key is the whole of
-- why relabelling an issue twice opens one Session.
CREATE TABLE firing (
    trigger_id TEXT NOT NULL REFERENCES trigger (id),
    event_id TEXT NOT NULL REFERENCES event (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    session_id TEXT NOT NULL REFERENCES session (id),
    fired_at TEXT NOT NULL,
    PRIMARY KEY (trigger_id, event_id)
) STRICT;

ALTER TABLE session ADD COLUMN event_id TEXT REFERENCES event (id);
