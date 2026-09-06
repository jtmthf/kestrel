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
    registered_at TEXT NOT NULL,
    UNIQUE (organization_id, name)
) STRICT;

CREATE INDEX integration_poll_due ON integration (poll_due_at) WHERE poll_due_at IS NOT NULL;

-- An Event is identified by what the external system calls it, and the same Event seen twice
-- in two overlapping poll windows is one row.
CREATE TABLE event (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    integration_id TEXT NOT NULL REFERENCES integration (id),
    external_id TEXT NOT NULL,
    repository TEXT NOT NULL,
    kind TEXT NOT NULL,
    actor TEXT NOT NULL,
    subject INTEGER NOT NULL,
    title TEXT NOT NULL,
    url TEXT NOT NULL,
    label TEXT,
    occurred_at TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE (integration_id, external_id)
) STRICT;

CREATE INDEX event_by_organization ON event (organization_id, occurred_at);
