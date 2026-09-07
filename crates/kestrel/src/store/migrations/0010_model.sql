-- An Agent that names no model runs on whatever its Agent Runtime defaults to, which is a
-- model it did not name rather than one named as nothing.
ALTER TABLE agent ADD COLUMN names_model TEXT;
UPDATE agent SET names_model = model WHERE model <> '';
ALTER TABLE agent DROP COLUMN model;
ALTER TABLE agent RENAME COLUMN names_model TO model;

ALTER TABLE run ADD COLUMN model TEXT;

CREATE TABLE runtime_model (
    organization_id TEXT NOT NULL REFERENCES organization (id),
    runtime TEXT NOT NULL,
    model TEXT NOT NULL,
    advertised_at TEXT NOT NULL,
    PRIMARY KEY (organization_id, runtime, model)
) STRICT;
