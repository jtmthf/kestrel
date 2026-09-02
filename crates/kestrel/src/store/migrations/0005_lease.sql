-- A Run holds a lease from the moment it is claimed until it ends, and what a sweep needs is
-- the due time rather than the moment its Environment was last heard from.
ALTER TABLE run DROP COLUMN heartbeat_at;
ALTER TABLE run ADD COLUMN lease_expires_at TEXT;

CREATE INDEX run_lease_due ON run (lease_expires_at) WHERE lease_expires_at IS NOT NULL;
