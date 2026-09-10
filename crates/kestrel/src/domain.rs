use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::integration::credential::Token;

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

identifiers!(
    OrganizationId,
    WorkspaceId,
    AgentId,
    SessionId,
    RunId,
    IntegrationId,
    EventId,
    TriggerId,
);

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
    /// None when the Agent names none, and the Agent Runtime's own default is the answer.
    pub model: Option<String>,
}

/// Which way an Integration carries: events inbound, kestrel's requests outbound, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::Inbound => "inbound",
            Direction::Outbound => "outbound",
        }
    }
}

impl FromStr for Direction {
    type Err = anyhow::Error;

    fn from_str(direction: &str) -> Result<Self> {
        match direction {
            "inbound" => Ok(Direction::Inbound),
            "outbound" => Ok(Direction::Outbound),
            other => bail!("{other} is not a direction an integration carries"),
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationKind {
    Github,
}

impl IntegrationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            IntegrationKind::Github => "github",
        }
    }
}

impl FromStr for IntegrationKind {
    type Err = anyhow::Error;

    fn from_str(kind: &str) -> Result<Self> {
        match kind {
            "github" => Ok(IntegrationKind::Github),
            other => bail!("{other} is not an external system kestrel integrates with"),
        }
    }
}

impl fmt::Display for IntegrationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Integration {
    pub id: IntegrationId,
    pub organization: OrganizationId,
    pub name: String,
    pub kind: IntegrationKind,
    pub repository: String,
    pub api: String,
    pub credential: Token,
    pub carries: Vec<Direction>,
    pub interval: SignedDuration,
    pub poll_due_at: Option<Timestamp>,
    pub polled_through: Option<i64>,
    pub comments_polled_through: Option<i64>,
}

impl Integration {
    pub fn carries(&self, direction: Direction) -> bool {
        self.carries.contains(&direction)
    }
}

/// A thing the external system says happened, as it arrives and before kestrel has decided
/// whether it has seen it before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub external_id: String,
    pub kind: String,
    pub actor: String,
    pub subject: i64,
    pub title: String,
    pub url: String,
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub occurred_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: EventId,
    pub organization: OrganizationId,
    pub integration: IntegrationId,
    pub repository: String,
    pub occurrence: Occurrence,
    pub recorded_at: Timestamp,
}

/// A Run's exit status on its way back to the surface that started the Session, composed when
/// the Run ended and posted once however many attempts that takes.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub run: RunId,
    pub organization: OrganizationId,
    pub integration: IntegrationId,
    pub event: EventId,
    pub subject: i64,
    pub body: String,
    /// Set before a request goes out and left set: a Run whose Session was told nothing yet
    /// but which has been attempted may already have a comment on the issue.
    pub attempted_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerState {
    Enabled,
    Disabled,
}

impl TriggerState {
    pub const fn as_str(self) -> &'static str {
        match self {
            TriggerState::Enabled => "enabled",
            TriggerState::Disabled => "disabled",
        }
    }
}

impl FromStr for TriggerState {
    type Err = anyhow::Error;

    fn from_str(state: &str) -> Result<Self> {
        match state {
            "enabled" => Ok(TriggerState::Enabled),
            "disabled" => Ok(TriggerState::Disabled),
            other => bail!("{other} is not a state a trigger can be in"),
        }
    }
}

impl fmt::Display for TriggerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The rule, never an individual firing.
#[derive(Debug, Clone)]
pub struct Trigger {
    pub id: TriggerId,
    pub organization: Organization,
    pub name: String,
    pub repository: String,
    pub label: String,
    pub workspace: Workspace,
    pub agent: Agent,
    pub state: TriggerState,
    pub declared_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: SessionId,
    pub organization: Organization,
    pub workspace: Workspace,
    pub agent: Agent,
    pub state: SessionState,
    pub opened_at: Timestamp,
    pub last_active_at: Timestamp,
    pub sealed_at: Option<Timestamp>,
    pub continues: Option<SessionId>,
    pub started_by: Option<EventId>,
}

impl Session {
    pub fn accepts(&self, what: &str) -> Result<()> {
        if self.state == SessionState::Sealed {
            bail!("the session {} is sealed, and accepts no {what}", self.id);
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Run {
    pub id: RunId,
    pub organization: OrganizationId,
    pub session: SessionId,
    pub state: RunState,
    pub exit: Option<Exit>,
    pub environment: Option<String>,
    /// What the Agent Runtime was on, once it has said; never what the Agent named.
    pub model: Option<String>,
    pub enqueued_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub lease_expires_at: Option<Timestamp>,
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
    /// Terminal like `Ended`, but with no exit status: a queued Run whose declared tolerance
    /// can no longer be met never ran, so nothing failed.
    Unreachable,
}

impl RunState {
    pub const fn as_str(self) -> &'static str {
        match self {
            RunState::Queued => "queued",
            RunState::Active => "active",
            RunState::Ended => "ended",
            RunState::Unreachable => "unreachable",
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
            "unreachable" => Ok(RunState::Unreachable),
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
