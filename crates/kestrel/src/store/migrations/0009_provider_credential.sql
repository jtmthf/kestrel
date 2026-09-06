CREATE TABLE provider_credential (
    organization_id TEXT NOT NULL REFERENCES organization (id),
    variable TEXT NOT NULL,
    sealed TEXT NOT NULL,
    set_at TEXT NOT NULL,
    PRIMARY KEY (organization_id, variable)
) STRICT;
