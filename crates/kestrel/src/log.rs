use std::fmt;

use anyhow::{Context as _, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};

use crate::domain::{Exit, RunId, Session};

/// What changed a Session's shared state. Never what happened inside a Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    ParticipantJoined {
        participant: String,
    },
    RunStarted {
        run: RunId,
    },
    Said {
        participant: String,
        message: String,
    },
    RunEnded {
        run: RunId,
        exit: Exit,
    },
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Entry::ParticipantJoined { participant } => {
                write!(f, "participant joined  {participant}")
            }
            Entry::RunStarted { run } => write!(f, "run started  {run}"),
            Entry::Said {
                participant,
                message,
            } => write!(f, "said  {participant}  {message}"),
            Entry::RunEnded { run, exit } => write!(f, "run ended  {run}  {exit}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub seq: i64,
    pub appended_at: Timestamp,
    pub entry: Entry,
}

pub struct Log<'a> {
    connection: &'a mut SqliteConnection,
}

impl<'a> Log<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection) -> Self {
        Self { connection }
    }

    pub async fn append(&mut self, session: &Session, entry: Entry) -> Result<TranscriptEntry> {
        let appended_at = Timestamp::now();

        let appended = sqlx::query(
            "INSERT INTO transcript_entry (session_id, organization_id, seq, body, appended_at)
             VALUES (
                 ?,
                 ?,
                 (SELECT COALESCE(MAX(seq), 0) + 1 FROM transcript_entry WHERE session_id = ?),
                 ?,
                 ?
             )
             RETURNING seq",
        )
        .bind(session.id.to_string())
        .bind(session.organization.id.to_string())
        .bind(session.id.to_string())
        .bind(serde_json::to_string(&entry)?)
        .bind(appended_at.to_string())
        .fetch_one(&mut *self.connection)
        .await
        .with_context(|| format!("appending to the transcript of session {}", session.id))?;

        Ok(TranscriptEntry {
            seq: appended.get("seq"),
            appended_at,
            entry,
        })
    }

    pub async fn transcript(&mut self, session: &Session) -> Result<Vec<TranscriptEntry>> {
        sqlx::query(
            "SELECT seq, body, appended_at
             FROM transcript_entry
             WHERE session_id = ?
             ORDER BY seq",
        )
        .bind(session.id.to_string())
        .fetch_all(&mut *self.connection)
        .await?
        .iter()
        .map(|row| {
            Ok(TranscriptEntry {
                seq: row.get("seq"),
                appended_at: row.get::<String, _>("appended_at").parse()?,
                entry: serde_json::from_str(row.get("body"))?,
            })
        })
        .collect()
    }
}
