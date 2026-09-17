CREATE TABLE trigger (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    name TEXT NOT NULL,
    filter TEXT,
    every_ms INTEGER CHECK (every_ms > 0),
    due_at TEXT,
    brief TEXT NOT NULL,
    branch TEXT,
    correlation TEXT,
    on_miss TEXT CHECK (on_miss IN ('open', 'ignore')),
    workspace_id TEXT NOT NULL REFERENCES workspace (id),
    agent_id TEXT NOT NULL REFERENCES agent (id),
    state TEXT NOT NULL,
    -- An apply removes only what an apply declared, never a one-off declared by flags.
    applied INTEGER NOT NULL CHECK (applied IN (0, 1)),
    enabled_at TEXT NOT NULL,
    declared_at TEXT NOT NULL,
    CHECK ((correlation IS NULL) = (on_miss IS NULL)),
    CHECK ((filter IS NULL) <> (every_ms IS NULL)),
    CHECK ((every_ms IS NULL) = (due_at IS NULL)),
    UNIQUE (organization_id, name)
) STRICT;

-- The key makes one trigger-event pair fire at most once, including failures and ignored misses.
CREATE TABLE firing (
    trigger_id TEXT NOT NULL REFERENCES trigger (id),
    event_record_id TEXT NOT NULL REFERENCES event (record_id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    session_id TEXT REFERENCES session (id),
    outcome TEXT NOT NULL CHECK (outcome IN ('opened', 'fed', 'ignored', 'failed')),
    failure TEXT,
    fired_at TEXT NOT NULL,
    PRIMARY KEY (trigger_id, event_record_id),
    CHECK ((outcome IN ('opened', 'fed')) = (session_id IS NOT NULL)),
    CHECK ((outcome = 'failed') = (failure IS NOT NULL))
) STRICT;

ALTER TABLE session ADD COLUMN event_record_id TEXT REFERENCES event (record_id);
ALTER TABLE session ADD COLUMN correlation TEXT;

CREATE UNIQUE INDEX session_open_correlation ON session (organization_id, correlation)
    WHERE state = 'open' AND correlation IS NOT NULL;
