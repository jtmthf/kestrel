use std::fmt;
use std::str::FromStr;

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};

use crate::domain::{Exit, RunId, Session, SessionId};

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
        session.accepts("new transcript entry")?;
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

    pub async fn page(
        &mut self,
        session: &Session,
        from: Option<Cursor>,
        window: Window,
    ) -> Result<Page, Unreadable> {
        let from = self.position(session, from).await?;

        Ok(self.after(session, from, window).await?)
    }

    async fn position(
        &mut self,
        session: &Session,
        cursor: Option<Cursor>,
    ) -> Result<Option<Cursor>, Unreadable> {
        let Some(cursor) = cursor else {
            return Ok(None);
        };

        if cursor.session != session.id {
            return Err(Unreadable::Cursor(format!(
                "the cursor {cursor} walks another transcript"
            )));
        }

        let known =
            sqlx::query("SELECT seq FROM transcript_entry WHERE session_id = ? AND seq = ?")
                .bind(session.id.to_string())
                .bind(cursor.seq)
                .fetch_optional(&mut *self.connection)
                .await?;

        match known {
            Some(_) => Ok(Some(cursor)),
            None => Err(Unreadable::Cursor(format!(
                "the cursor {cursor} is no position in this transcript"
            ))),
        }
    }

    /// One entry beyond the window is read and dropped, which is what tells a reader whether
    /// more are waiting without asking a second time.
    async fn after(
        &mut self,
        session: &Session,
        from: Option<Cursor>,
        window: Window,
    ) -> Result<Page> {
        let rows = sqlx::query(
            "SELECT seq, body, appended_at
             FROM transcript_entry
             WHERE session_id = ? AND seq > ?
             ORDER BY seq
             LIMIT ?",
        )
        .bind(session.id.to_string())
        .bind(from.map_or(0, |cursor| cursor.seq))
        .bind(i64::try_from(window.0 + 1)?)
        .fetch_all(&mut *self.connection)
        .await
        .with_context(|| format!("reading the transcript of session {}", session.id))?;

        let more = rows.len() > window.0;
        let entries = rows
            .iter()
            .take(window.0)
            .map(|row| {
                Ok(TranscriptEntry {
                    seq: row.get("seq"),
                    appended_at: row.get::<String, _>("appended_at").parse()?,
                    entry: serde_json::from_str(row.get("body"))?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Page {
            cursor: entries
                .last()
                .map(|entry| Cursor {
                    session: session.id,
                    seq: entry.seq,
                })
                .or(from),
            entries,
            more,
        })
    }
}

pub struct Page {
    pub entries: Vec<TranscriptEntry>,
    pub cursor: Option<Cursor>,
    pub more: bool,
}

/// A position in one Transcript rather than a handle the control plane holds open, so it
/// still walks after the process that issued it is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    session: SessionId,
    seq: i64,
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.session, self.seq)
    }
}

impl FromStr for Cursor {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let unreadable = || format!("{text} is no cursor");
        let (session, seq) = text.split_once(':').with_context(unreadable)?;

        Ok(Self {
            session: session.parse().with_context(unreadable)?,
            seq: seq.parse().with_context(unreadable)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window(usize);

impl Window {
    pub const DEFAULT: Self = Self(100);
    const MOST: usize = 500;

    pub fn or_default(entries: Option<usize>) -> Result<Self> {
        entries.map_or(Ok(Self::DEFAULT), Self::of)
    }

    pub fn of(entries: usize) -> Result<Self> {
        if entries == 0 || entries > Self::MOST {
            bail!("a window is 1 to {} entries, not {entries}", Self::MOST);
        }

        Ok(Self(entries))
    }
}

#[derive(Debug)]
pub enum Unreadable {
    /// Resuming from the beginning instead would hand the reader what it has already walked,
    /// as though it were new.
    Cursor(String),
    Unavailable(anyhow::Error),
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unreadable::Cursor(why) => f.write_str(why),
            Unreadable::Unavailable(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Unreadable {}

impl From<anyhow::Error> for Unreadable {
    fn from(error: anyhow::Error) -> Self {
        Unreadable::Unavailable(error)
    }
}

impl From<sqlx::Error> for Unreadable {
    fn from(error: sqlx::Error) -> Self {
        Unreadable::Unavailable(error.into())
    }
}
