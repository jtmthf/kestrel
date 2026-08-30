use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use jiff::Timestamp;
use uuid::Uuid;

macro_rules! identifiers {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub connected: Option<Connected>,
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
