-- A Workspace that predates the idle sweep was last active when it opened.
ALTER TABLE workspace ADD COLUMN last_active_at TEXT NOT NULL DEFAULT '';
UPDATE workspace SET last_active_at = opened_at;

CREATE INDEX workspace_idle ON workspace (state, last_active_at);
