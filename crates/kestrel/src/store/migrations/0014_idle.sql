-- A Session that predates the idle sweep was last active when it opened.
ALTER TABLE session ADD COLUMN last_active_at TEXT NOT NULL DEFAULT '';
UPDATE session SET last_active_at = opened_at;

CREATE INDEX session_idle ON session (state, last_active_at);
