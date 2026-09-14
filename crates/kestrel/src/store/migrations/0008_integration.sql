CREATE TABLE integration (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    repository TEXT NOT NULL,
    api TEXT NOT NULL,
    credential TEXT NOT NULL,
    inbound INTEGER NOT NULL,
    outbound INTEGER NOT NULL,
    interval_ms INTEGER NOT NULL,
    poll_due_at TEXT,
    polled_through INTEGER,
    last_event_refusal_source TEXT,
    last_event_refusal_id TEXT,
    last_event_refusal_bytes INTEGER,
    last_event_refusal_reason TEXT,
    last_event_refusal_at TEXT,
    registered_at TEXT NOT NULL,
    UNIQUE (organization_id, name)
) STRICT;

CREATE INDEX integration_poll_due ON integration (poll_due_at) WHERE poll_due_at IS NOT NULL;

CREATE TABLE event (
    record_id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    integration_id TEXT NOT NULL REFERENCES integration (id),
    id TEXT NOT NULL CHECK (id <> ''),
    source TEXT NOT NULL CHECK (source <> ''),
    specversion TEXT NOT NULL CHECK (specversion = '1.0'),
    type TEXT NOT NULL CHECK (type <> ''),
    subject TEXT,
    time TEXT NOT NULL,
    data TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE (organization_id, source, id)
) STRICT;

CREATE INDEX event_by_organization ON event (organization_id, time);
