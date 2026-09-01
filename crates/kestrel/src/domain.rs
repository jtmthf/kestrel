use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! identifiers {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(Uuid);

        impl $name {
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Ok(Self(text.parse()?))
            }
        }
    )+};
}

identifiers!(OrganizationId, WorkspaceId, AgentId, SessionId, RunId);

#[derive(Debug, Clone)]
pub struct Organization {
    pub id: OrganizationId,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub organization: OrganizationId,
    pub name: String,
    pub repositories: Vec<String>,
    pub branch: String,
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub id: AgentId,
    pub organization: OrganizationId,
    pub name: String,
    pub runtime: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: SessionId,
    pub organization: Organization,
    pub workspace: Workspace,
    pub agent: Agent,
    pub state: SessionState,
    pub opened_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct Run {
    pub id: RunId,
    pub organization: OrganizationId,
    pub session: SessionId,
    pub state: RunState,
    pub exit: Option<Exit>,
    pub environment: Option<String>,
    pub enqueued_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub heartbeat_at: Option<Timestamp>,
    pub connected: Option<Connected>,
    pub usage: Option<Usage>,
}

/// What the Agent Runtime has spent on behalf of a Run, cumulative rather than per turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub context_used: u64,
    pub context_size: u64,
    pub cost: Option<Cost>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
}

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} of {} tokens", self.context_used, self.context_size)?;
        match &self.cost {
            Some(cost) => write!(f, ", {:.2} {}", cost.amount, cost.currency),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Queued,
    Active,
    Ended,
}

impl RunState {
    pub const fn as_str(self) -> &'static str {
        match self {
            RunState::Queued => "queued",
            RunState::Active => "active",
            RunState::Ended => "ended",
        }
    }
}

impl FromStr for RunState {
    type Err = anyhow::Error;

    fn from_str(state: &str) -> Result<Self> {
        match state {
            "queued" => Ok(RunState::Queued),
            "active" => Ok(RunState::Active),
            "ended" => Ok(RunState::Ended),
            other => bail!("{other} is not a state a run can be in"),
        }
    }
}

impl fmt::Display for RunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Exit {
    Succeeded,
    Failed { because: String },
}

impl Exit {
    pub const fn status(&self) -> &'static str {
        match self {
            Exit::Succeeded => "succeeded",
            Exit::Failed { .. } => "failed",
        }
    }

    pub fn because(&self) -> Option<&str> {
        match self {
            Exit::Succeeded => None,
            Exit::Failed { because } => Some(because),
        }
    }

    pub fn read(status: &str, because: Option<String>) -> Result<Self> {
        match status {
            "succeeded" => Ok(Exit::Succeeded),
            "failed" => Ok(Exit::Failed {
                because: because.unwrap_or_default(),
            }),
            other => bail!("{other} is not an exit status a run can end with"),
        }
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Exit::Succeeded => f.write_str("succeeded"),
            Exit::Failed { because } => write!(f, "failed: {because}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Connected {
    pub at: Timestamp,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Open,
    Sealed,
}

impl SessionState {
    pub const fn as_str(self) -> &'static str {
        match self {
            SessionState::Open => "open",
            SessionState::Sealed => "sealed",
        }
    }
}

impl FromStr for SessionState {
    type Err = anyhow::Error;

    fn from_str(state: &str) -> Result<Self> {
        match state {
            "open" => Ok(SessionState::Open),
            "sealed" => Ok(SessionState::Sealed),
            other => bail!("{other} is not a state a session can be in"),
        }
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
