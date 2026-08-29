CREATE TABLE organization (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    declared_at TEXT NOT NULL
) STRICT;

CREATE TABLE workspace (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    name TEXT NOT NULL,
    branch TEXT NOT NULL,
    declared_at TEXT NOT NULL,
    UNIQUE (organization_id, name)
) STRICT;

CREATE TABLE workspace_repository (
    workspace_id TEXT NOT NULL REFERENCES workspace (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    position INTEGER NOT NULL,
    url TEXT NOT NULL,
    PRIMARY KEY (workspace_id, position)
) STRICT;

CREATE TABLE agent (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    name TEXT NOT NULL,
    runtime TEXT NOT NULL,
    model TEXT NOT NULL,
    declared_at TEXT NOT NULL,
    UNIQUE (organization_id, name)
) STRICT;

CREATE TABLE session (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    workspace_id TEXT NOT NULL REFERENCES workspace (id),
    agent_id TEXT NOT NULL REFERENCES agent (id),
    state TEXT NOT NULL,
    opened_at TEXT NOT NULL
) STRICT;

CREATE TABLE transcript_entry (
    session_id TEXT NOT NULL REFERENCES session (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    seq INTEGER NOT NULL,
    body TEXT NOT NULL,
    appended_at TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
) STRICT;
