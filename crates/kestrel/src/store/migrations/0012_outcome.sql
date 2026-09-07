-- One row per Run whose Session an Event started: what kestrel says back where the work came
-- from, composed when the Run ends and posted afterwards. `attempted_at` is set before the
-- request goes out and left set, so a control plane that died mid-post finds a row that says
-- a comment may already be there and reads it back rather than posting a second one.
CREATE TABLE outcome (
    run_id TEXT PRIMARY KEY REFERENCES run (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    integration_id TEXT NOT NULL REFERENCES integration (id),
    event_id TEXT NOT NULL REFERENCES event (id),
    subject INTEGER NOT NULL,
    body TEXT NOT NULL,
    attempted_at TEXT,
    due_at TEXT,
    delivered_at TEXT,
    delivered_to TEXT,
    recorded_at TEXT NOT NULL
) STRICT;

CREATE INDEX outcome_due ON outcome (due_at) WHERE due_at IS NOT NULL;
