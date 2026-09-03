-- A Session that predates sealing is open, and continues nothing.
ALTER TABLE session ADD COLUMN sealed_at TEXT;
ALTER TABLE session ADD COLUMN continues TEXT REFERENCES session (id);

CREATE INDEX session_continued_by ON session (continues) WHERE continues IS NOT NULL;

CREATE INDEX run_holding_a_slot ON run (session_id, state);
