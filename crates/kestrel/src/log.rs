use std::fmt;
use std::str::FromStr;

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};

use crate::domain::{Exit, RunId, Workspace, WorkspaceId, WorkspaceState};

/// What changed a Workspace's shared state. Never what happened inside a Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    ParticipantJoined {
        participant: String,
    },
    Brief {
        trigger: Option<String>,
        brief: String,
    },
    RunStarted {
        run: RunId,
    },
    Said {
        participant: String,
        message: String,
    },
    Messages {
        messages: Vec<Message>,
    },
    RunEnded {
        run: RunId,
        exit: Exit,
    },
    InstanceReleased {
        participant: String,
        instance: String,
        unpublished: Option<String>,
    },
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Entry::ParticipantJoined { participant } => {
                write!(f, "participant joined  {participant}")
            }
            Entry::Brief {
                trigger: Some(trigger),
                brief,
            } => write!(f, "brief  {trigger}  {brief}"),
            Entry::Brief {
                trigger: None,
                brief,
            } => write!(f, "brief  {brief}"),
            Entry::RunStarted { run } => write!(f, "run started  {run}"),
            Entry::Said {
                participant,
                message,
            } => write!(f, "said  {participant}  {message}"),
            Entry::Messages { messages } => write!(
                f,
                "messages  {}",
                messages
                    .iter()
                    .map(|message| format!("{}  {}", message.participant, message.message))
                    .collect::<Vec<_>>()
                    .join("  ")
            ),
            Entry::RunEnded { run, exit } => write!(f, "run ended  {run}  {exit}"),
            Entry::InstanceReleased {
                participant,
                instance,
                unpublished,
            } => {
                write!(f, "instance released  {participant}  {instance}")?;
                match unpublished {
                    Some(unpublished) => write!(f, "  discarding {unpublished}"),
                    None => Ok(()),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub participant: String,
    pub message: String,
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

    /// The state is read in the statement that appends rather than off the `Workspace` handed
    /// in, so one sealed after the caller read it refuses all the same.
    pub async fn append(&mut self, workspace: &Workspace, entry: Entry) -> Result<TranscriptEntry> {
        let appended_at = Timestamp::now();

        let appended = sqlx::query(
            "INSERT INTO transcript_entry (workspace_id, organization_id, seq, body, appended_at)
             SELECT
                 ?,
                 ?,
                 (SELECT COALESCE(MAX(seq), 0) + 1 FROM transcript_entry WHERE workspace_id = ?),
                 ?,
                 ?
             WHERE EXISTS (SELECT 1 FROM workspace WHERE id = ? AND state = ?)
             RETURNING seq",
        )
        .bind(workspace.id.to_string())
        .bind(workspace.organization.id.to_string())
        .bind(workspace.id.to_string())
        .bind(serde_json::to_string(&entry)?)
        .bind(appended_at.to_string())
        .bind(workspace.id.to_string())
        .bind(WorkspaceState::Open.as_str())
        .fetch_optional(&mut *self.connection)
        .await
        .with_context(|| format!("appending to the transcript of workspace {}", workspace.id))?
        .with_context(|| {
            format!(
                "the workspace {} is sealed, and accepts no new transcript entry",
                workspace.id
            )
        })?;

        Ok(TranscriptEntry {
            seq: appended.get("seq"),
            appended_at,
            entry,
        })
    }

    pub async fn last_said_for_run(&mut self, workspace: &Workspace) -> Result<Option<String>> {
        let latest = sqlx::query(
            "SELECT body
             FROM transcript_entry
             WHERE workspace_id = ? AND json_extract(body, '$.kind') = 'said'
               AND json_extract(body, '$.participant') = ?
               AND seq > COALESCE((
                   SELECT MAX(seq) FROM transcript_entry
                   WHERE workspace_id = ? AND json_extract(body, '$.kind') = 'run_ended'
               ), 0)
             ORDER BY seq DESC
             LIMIT 1",
        )
        .bind(workspace.id.to_string())
        .bind(&workspace.agent.name)
        .bind(workspace.id.to_string())
        .fetch_optional(&mut *self.connection)
        .await
        .with_context(|| format!("reading the transcript of workspace {}", workspace.id))?;

        let Some(row) = latest else {
            return Ok(None);
        };

        Ok(match serde_json::from_str(row.get("body"))? {
            Entry::Said {
                participant,
                message,
            } if participant == workspace.agent.name => Some(message),
            _ => None,
        })
    }

    /// What one participant said after a Turn was prompted, oldest first, which is that Turn's
    /// response to report.
    pub async fn said_since(
        &mut self,
        workspace: &Workspace,
        seq: i64,
        participant: &str,
    ) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT body
             FROM transcript_entry
             WHERE workspace_id = ? AND seq > ?
               AND json_extract(body, '$.kind') = 'said'
             ORDER BY seq",
        )
        .bind(workspace.id.to_string())
        .bind(seq)
        .fetch_all(&mut *self.connection)
        .await
        .with_context(|| format!("reading what was said in workspace {}", workspace.id))?;

        let mut said = Vec::new();
        for row in rows {
            if let Entry::Said {
                participant: who,
                message,
            } = serde_json::from_str(row.get("body"))?
                && who == participant
            {
                said.push(message);
            }
        }

        Ok(said)
    }

    /// The Brief, if nobody has said anything since it: participants joining and Runs starting
    /// are not something said.
    pub async fn unfollowed_brief(&mut self, workspace: &Workspace) -> Result<Option<String>> {
        let said = sqlx::query(
            "SELECT body
             FROM transcript_entry
             WHERE workspace_id = ?
               AND json_extract(body, '$.kind') NOT IN ('participant_joined', 'run_started')
             ORDER BY seq
             LIMIT 2",
        )
        .bind(workspace.id.to_string())
        .fetch_all(&mut *self.connection)
        .await
        .with_context(|| format!("reading the transcript of workspace {}", workspace.id))?;

        let [only] = said.as_slice() else {
            return Ok(None);
        };
        Ok(match serde_json::from_str(only.get("body"))? {
            Entry::Brief { brief, .. } => Some(brief),
            _ => None,
        })
    }

    pub async fn page(
        &mut self,
        workspace: &Workspace,
        from: Option<Cursor>,
        window: Window,
    ) -> Result<Page, Unreadable> {
        let from = self.position(workspace, from).await?;

        Ok(self.after(workspace, from, window).await?)
    }

    async fn position(
        &mut self,
        workspace: &Workspace,
        cursor: Option<Cursor>,
    ) -> Result<Option<Cursor>, Unreadable> {
        let Some(cursor) = cursor else {
            return Ok(None);
        };

        if cursor.workspace != workspace.id {
            return Err(Unreadable::Cursor(format!(
                "the cursor {cursor} walks another transcript"
            )));
        }

        let known =
            sqlx::query("SELECT seq FROM transcript_entry WHERE workspace_id = ? AND seq = ?")
                .bind(workspace.id.to_string())
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
        workspace: &Workspace,
        from: Option<Cursor>,
        window: Window,
    ) -> Result<Page> {
        let rows = sqlx::query(
            "SELECT seq, body, appended_at
             FROM transcript_entry
             WHERE workspace_id = ? AND seq > ?
             ORDER BY seq
             LIMIT ?",
        )
        .bind(workspace.id.to_string())
        .bind(from.map_or(0, |cursor| cursor.seq))
        .bind(i64::try_from(window.0 + 1)?)
        .fetch_all(&mut *self.connection)
        .await
        .with_context(|| format!("reading the transcript of workspace {}", workspace.id))?;

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
                    workspace: workspace.id,
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
    workspace: WorkspaceId,
    seq: i64,
}

impl Cursor {
    pub const fn at(workspace: WorkspaceId, seq: i64) -> Self {
        Self { workspace, seq }
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.workspace, self.seq)
    }
}

impl FromStr for Cursor {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let unreadable = || format!("{text} is no cursor");
        let (workspace, seq) = text.split_once(':').with_context(unreadable)?;

        Ok(Self {
            workspace: workspace.parse().with_context(unreadable)?,
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
