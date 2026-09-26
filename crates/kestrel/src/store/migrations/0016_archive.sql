-- An Instance leaves its Workspace in the transaction that seals or releases it, and waits here for
-- a work role to destroy it, so a work role that is down when a Workspace seals still finds it.
CREATE TABLE instance_archive (
    instance TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    workspace_id TEXT NOT NULL REFERENCES workspace (id),
    queued_at TEXT NOT NULL
) STRICT;
