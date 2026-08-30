ALTER TABLE run RENAME COLUMN started_at TO enqueued_at;

-- Every Run inserted from here on names its own state; the default is what a Run that
-- predates kestrel scheduling one gets, and such a Run is over.
ALTER TABLE run ADD COLUMN state TEXT NOT NULL DEFAULT 'ended';
ALTER TABLE run ADD COLUMN claimed_at TEXT;
ALTER TABLE run ADD COLUMN started_at TEXT;
ALTER TABLE run ADD COLUMN heartbeat_at TEXT;
ALTER TABLE run ADD COLUMN environment TEXT;
ALTER TABLE run ADD COLUMN exit TEXT;
ALTER TABLE run ADD COLUMN exit_because TEXT;
