CREATE TABLE subscription_profile (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    name TEXT NOT NULL,
    owner TEXT NOT NULL,
    declared_at TEXT NOT NULL,
    UNIQUE (organization_id, name)
) STRICT;

CREATE TABLE subscription_profile_entry (
    profile_id TEXT NOT NULL REFERENCES subscription_profile (id),
    organization_id TEXT NOT NULL REFERENCES organization (id),
    kind TEXT NOT NULL CHECK (kind IN ('variable', 'file')),
    name TEXT NOT NULL,
    sealed TEXT NOT NULL,
    set_at TEXT NOT NULL,
    PRIMARY KEY (profile_id, kind, name)
) STRICT;

ALTER TABLE session ADD COLUMN subscription_profile_id TEXT REFERENCES subscription_profile (id);
ALTER TABLE trigger ADD COLUMN subscription_profile_id TEXT REFERENCES subscription_profile (id);
