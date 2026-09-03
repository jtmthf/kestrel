-- The Environment numbers its ordered account of a turn, and the control plane takes each
-- number once. A Run that predates the numbering has reported nothing under it, and is over.
ALTER TABLE run ADD COLUMN reports_taken INTEGER NOT NULL DEFAULT 0;
