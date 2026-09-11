-- no-transaction

-- An Event is a CloudEvent (ADR-0011): id, source, type, subject, time, and the whole
-- payload as data. The flattened columns a poller needed are gone, and dedup is on
-- (source, external_id) rather than on the integration that happened to record it. The
-- rebuild is statement-by-statement -- building, filling, dropping and renaming in one
-- transaction of its own -- with foreign keys put aside so child rows pointing at an Event
-- are not in the way of the table that starts carrying it.
PRAGMA foreign_keys = OFF;

CREATE TABLE event_new (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organization (id),
    integration_id TEXT NOT NULL REFERENCES integration (id),
    source TEXT NOT NULL,
    external_id TEXT NOT NULL,
    type TEXT NOT NULL,
    subject TEXT,
    time TEXT NOT NULL,
    data TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE (source, external_id)
) STRICT;

INSERT INTO event_new (id, organization_id, integration_id, source, external_id, type,
                       subject, time, data, recorded_at)
SELECT id,
       organization_id,
       integration_id,
       'https://github.com/' || repository,
       external_id,
       CASE
           WHEN kind = 'commented' THEN 'com.github.issue_comment.created'
           ELSE 'com.github.issues.' || kind
       END,
       '#' || subject,
       occurred_at,
       json_object(
           'id', external_id,
           'event', kind,
           'actor', json_object('login', actor),
           'label', CASE WHEN label IS NULL THEN NULL ELSE json_object('name', label) END,
           'issue', json_object('number', subject, 'title', title, 'html_url', url),
           'message', message,
           'created_at', occurred_at
       ),
       recorded_at
FROM event;

DROP TABLE event;

ALTER TABLE event_new RENAME TO event;

CREATE INDEX event_by_organization ON event (organization_id, time);
CREATE INDEX event_recorded ON event (recorded_at);

PRAGMA foreign_keys = ON;