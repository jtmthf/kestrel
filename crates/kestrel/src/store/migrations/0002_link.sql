CREATE TABLE run (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    session_id TEXT NOT NULL REFERENCES session (id),
    started_at TEXT NOT NULL,
    ended_at TEXT,
    connected_at TEXT,
    supervisor_version TEXT
) STRICT;

CREATE TABLE run_credential (
    token_hash TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES run (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    issued_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    invalidated_at TEXT
) STRICT;

CREATE TABLE link_instruction (
    run_id TEXT NOT NULL REFERENCES run (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    seq INTEGER NOT NULL,
    body TEXT NOT NULL,
    sent_at TEXT NOT NULL,
    PRIMARY KEY (run_id, seq)
) STRICT;
