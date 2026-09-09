CREATE TABLE run_dependency (
    run_id TEXT NOT NULL REFERENCES run (id),
    blocker_id TEXT NOT NULL REFERENCES run (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    PRIMARY KEY (run_id, blocker_id)
) STRICT;
