use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;

use anyhow::{Result, bail};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::filter::{Attribute, Filter};
use crate::integration::credential::Token;
use crate::template::Template;

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
    EventRecordId,
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
    Webhook,
}

impl IntegrationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            IntegrationKind::Github => "github",
            IntegrationKind::Webhook => "webhook",
        }
    }
}

impl FromStr for IntegrationKind {
    type Err = anyhow::Error;

    fn from_str(kind: &str) -> Result<Self> {
        match kind {
            "github" => Ok(IntegrationKind::Github),
            "webhook" => Ok(IntegrationKind::Webhook),
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
    pub connection: Connection,
    pub carries: Vec<Direction>,
    pub poll_due_at: Option<Timestamp>,
    pub polled_through: Option<i64>,
    pub comments_polled_through: Option<i64>,
    pub last_event_refusal: Option<EventRefusal>,
}

impl Integration {
    pub fn carries(&self, direction: Direction) -> bool {
        self.carries.contains(&direction)
    }

    pub const fn kind(&self) -> IntegrationKind {
        match self.connection {
            Connection::Github(_) => IntegrationKind::Github,
            Connection::Webhook => IntegrationKind::Webhook,
        }
    }

    pub fn github(&self) -> Result<&GithubConnection> {
        match &self.connection {
            Connection::Github(github) => Ok(github),
            Connection::Webhook => bail!("the integration {} is not a github one", self.name),
        }
    }

    pub fn webhook_path(&self) -> String {
        format!("/webhooks/{}", self.id)
    }
}

#[derive(Debug, Clone)]
pub enum Connection {
    Github(GithubConnection),
    Webhook,
}

#[derive(Debug, Clone)]
pub struct GithubConnection {
    pub repository: String,
    pub api: String,
    pub credential: Token,
    pub interval: SignedDuration,
    /// Delivered by a signed webhook rather than polled.
    pub signed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub id: String,
    pub source: String,
    pub specversion: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub subject: Option<String>,
    pub time: Timestamp,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub record_id: EventRecordId,
    pub organization: OrganizationId,
    /// None for an Event kestrel minted itself.
    pub integration: Option<IntegrationId>,
    pub occurrence: Occurrence,
    pub recorded_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct EventRefusal {
    pub source: String,
    pub id: String,
    pub bytes: usize,
    pub reason: String,
    pub observed_at: Timestamp,
}

/// A Run's exit status on its way back to the surface that started the Session, composed when
/// the Run ended and posted once however many attempts that takes.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub run: RunId,
    pub organization: OrganizationId,
    pub integration: IntegrationId,
    pub event: EventRecordId,
    pub subject: i64,
    pub body: String,
    /// Set before a request goes out and left set: a Run whose Session was told nothing yet
    /// but which has been attempted may already have a comment on the issue.
    pub attempted_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerState {
    Enabled,
    Disabled(DisableReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisableReason {
    Operator,
    FiringBudget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrelationMiss {
    Open,
    Ignore,
}

impl CorrelationMiss {
    pub const fn as_str(self) -> &'static str {
        match self {
            CorrelationMiss::Open => "open",
            CorrelationMiss::Ignore => "ignore",
        }
    }
}

impl FromStr for CorrelationMiss {
    type Err = anyhow::Error;

    fn from_str(miss: &str) -> Result<Self> {
        match miss {
            "open" => Ok(CorrelationMiss::Open),
            "ignore" => Ok(CorrelationMiss::Ignore),
            other => bail!("{other} is not what a trigger does when correlation misses"),
        }
    }
}

impl fmt::Display for CorrelationMiss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TriggerState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            TriggerState::Enabled => "enabled",
            TriggerState::Disabled(DisableReason::Operator) => "disabled:operator",
            TriggerState::Disabled(DisableReason::FiringBudget) => "disabled:firing-budget",
        }
    }
}

impl FromStr for TriggerState {
    type Err = anyhow::Error;

    fn from_str(state: &str) -> Result<Self> {
        match state {
            "enabled" => Ok(TriggerState::Enabled),
            "disabled:operator" => Ok(TriggerState::Disabled(DisableReason::Operator)),
            "disabled:firing-budget" => Ok(TriggerState::Disabled(DisableReason::FiringBudget)),
            other => bail!("{other} is not a state a trigger can be in"),
        }
    }
}

impl fmt::Display for TriggerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TriggerState::Enabled => f.write_str("enabled"),
            TriggerState::Disabled(_) => f.write_str("disabled"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FiringBudget {
    pub limit: NonZeroUsize,
    pub window: SignedDuration,
}

impl Default for FiringBudget {
    fn default() -> Self {
        Self {
            limit: NonZeroUsize::new(10).expect("a firing budget has a positive limit"),
            window: SignedDuration::from_hours(1),
        }
    }
}

/// The rule, never an individual firing.
#[derive(Debug, Clone)]
pub struct Trigger {
    pub id: TriggerId,
    pub organization: Organization,
    pub name: String,
    pub fires: Fires,
    pub templates: Templates,
    pub on_miss: Option<CorrelationMiss>,
    pub workspace: Workspace,
    pub agent: Agent,
    pub state: TriggerState,
    pub disabled_because: Option<String>,
    pub firing_budget: FiringBudget,
    pub applied: bool,
    pub declared_at: Timestamp,
}

impl Trigger {
    /// A scheduled Trigger matches only the Events its own elapsing mints, which is what puts
    /// scheduled work on the one firing path.
    pub fn filter(&self) -> Filter {
        match &self.fires {
            Fires::On(filter) => filter.clone(),
            Fires::Every(_) => Filter::All(vec![
                Filter::Exact(Attribute::Source, self.source()),
                Filter::Exact(Attribute::Type, ELAPSED.to_owned()),
            ]),
        }
    }

    /// The Event its schedule mints on elapsing when due, keyed by that due time so a sweep
    /// that runs twice records it once.
    pub fn elapsing(&self, due: Timestamp) -> Option<Occurrence> {
        let Fires::Every(every) = self.fires else {
            return None;
        };

        Some(Occurrence {
            id: due.to_string(),
            source: self.source(),
            specversion: "1.0".to_owned(),
            r#type: ELAPSED.to_owned(),
            subject: None,
            time: due,
            data: serde_json::json!({
                "trigger": self.name,
                "every": format!("{every:#}"),
            }),
        })
    }

    fn source(&self) -> String {
        format!("urn:kestrel:trigger:{}", self.id)
    }

    pub fn firing_budget_exhausted_because(&self) -> String {
        format!(
            "the trigger {} exhausted its budget of {} firings in {}",
            self.name, self.firing_budget.limit, self.firing_budget.window
        )
    }
}

/// What kestrel calls a schedule elapsing.
pub const ELAPSED: &str = "dev.kestrel.schedule.elapsed";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fires {
    On(Filter),
    Every(SignedDuration),
}

impl fmt::Display for Fires {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fires::On(filter) => filter.fmt(f),
            Fires::Every(every) => write!(f, "every {every:#}"),
        }
    }
}

/// What a firing renders from the Event: never the Agent or the Workspace (ADR-0013).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Templates {
    pub brief: Template,
    pub branch: Option<Template>,
    pub correlation: Option<Template>,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: SessionId,
    pub organization: Organization,
    pub workspace: Workspace,
    pub agent: Agent,
    pub branch: String,
    pub correlation: Option<String>,
    pub state: SessionState,
    pub opened_at: Timestamp,
    pub last_active_at: Timestamp,
    pub sealed_at: Option<Timestamp>,
    pub continues: Option<SessionId>,
    pub started_by: Option<EventRecordId>,
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
    /// What this Run names, or none for its Agent's or Agent Runtime's default.
    pub model: Option<String>,
    /// What the Agent Runtime reported it worked on.
    pub worked_model: Option<String>,
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
