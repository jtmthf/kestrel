CREATE TABLE organization (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    max_live_instances INTEGER CHECK (max_live_instances > 0),
    declared_at TEXT NOT NULL
) STRICT;

CREATE TABLE project (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    name TEXT NOT NULL,
    branch TEXT NOT NULL,
    declared_at TEXT NOT NULL,
    UNIQUE (organization_id, name)
) STRICT;

CREATE TABLE project_repository (
    project_id TEXT NOT NULL REFERENCES project (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    position INTEGER NOT NULL,
    url TEXT NOT NULL,
    PRIMARY KEY (project_id, position)
) STRICT;

CREATE TABLE agent (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    name TEXT NOT NULL,
    harness TEXT NOT NULL,
    model TEXT NOT NULL,
    declared_at TEXT NOT NULL,
    UNIQUE (organization_id, name)
) STRICT;

CREATE TABLE workspace (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    project_id TEXT NOT NULL REFERENCES project (id),
    agent_id TEXT NOT NULL REFERENCES agent (id),
    harness TEXT NOT NULL,
    base TEXT NOT NULL,
    branch TEXT NOT NULL,
    instance TEXT,
    observed TEXT,
    state TEXT NOT NULL,
    opened_at TEXT NOT NULL,
    UNIQUE (organization_id, name)
) STRICT;

CREATE TABLE workspace_repository (
    workspace_id TEXT NOT NULL REFERENCES workspace (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    position INTEGER NOT NULL,
    url TEXT NOT NULL,
    PRIMARY KEY (workspace_id, position)
) STRICT;

CREATE TABLE transcript_entry (
    workspace_id TEXT NOT NULL REFERENCES workspace (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    seq INTEGER NOT NULL,
    body TEXT NOT NULL,
    appended_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, seq)
) STRICT;
