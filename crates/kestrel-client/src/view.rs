//! What each command shows, and the three shapes it can be shown in.

/// What a command produces, as dotted paths into the record the control plane answered.
pub enum View {
    /// The one field a `$(…)` captures.
    Value(&'static str),
    Rows(&'static [&'static str]),
    Detail(&'static [&'static str]),
}

impl View {
    pub fn fields(&self) -> &[&'static str] {
        match self {
            View::Value(field) => std::slice::from_ref(field),
            View::Rows(fields) | View::Detail(fields) => fields,
        }
    }
}

pub const ORGANIZATIONS: View = View::Rows(&["id", "name"]);
pub const WORKSPACES: View = View::Rows(&["id", "name", "branch", "repositories"]);
pub const AGENTS: View = View::Rows(&["id", "name", "runtime", "model"]);
pub const CREDENTIAL: View = View::Value("variable");
pub const CREDENTIALS: View = View::Rows(&["variable", "set_at"]);
pub const PROFILES: View = View::Rows(&["id", "name", "owner", "holds"]);
pub const LOGIN: View = View::Value("name");
pub const INTEGRATIONS: View = View::Rows(&[
    "id",
    "name",
    "kind",
    "repository",
    "carries",
    "polled_every",
    "webhook_path",
    "last_event_refusal.reason",
]);
pub const EVENTS: View = View::Rows(&[
    "record",
    "event.time",
    "event.source",
    "event.type",
    "event.subject",
]);
pub const EVENT: View = View::Detail(&[
    "record",
    "organization",
    "integration",
    "recorded_at",
    "event.id",
    "event.specversion",
    "event.source",
    "event.type",
    "event.subject",
    "event.time",
    "event.data",
]);
pub const TRIGGERS: View = View::Rows(&[
    "id",
    "name",
    "state",
    "workspace",
    "agent",
    "every",
    "filter",
]);
pub const TRIGGER: View = View::Detail(&[
    "id",
    "organization",
    "name",
    "state",
    "disabled_because",
    "every",
    "filter",
    "workspace",
    "agent",
    "allows",
    "profile",
    "branch",
    "correlation",
    "on_miss",
    "declared_at",
    "brief",
]);
pub const TRIGGER_TEST: View = View::Detail(&[
    "matches",
    "elapsing",
    "agent",
    "branch",
    "correlation",
    "brief",
]);
pub const TRIGGER_STATE: View = View::Value("state");
pub const SESSIONS: View = View::Rows(&["id", "name", "state", "workspace", "agent", "started_by"]);
pub const ENTRIES: View = View::Rows(&["seq", "appended_at", "entry"]);
pub const SESSION: View = View::Detail(&[
    "id",
    "name",
    "organization",
    "workspace",
    "agent",
    "profile",
    "checkout.base",
    "checkout.branch",
    "correlation",
    "state",
    "opened_at",
    "last_active_at",
    "sealed_at",
    "started_by",
    "continues",
    "continued_by",
]);
pub const RUNS: View = View::Rows(&[
    "id",
    "name",
    "state",
    "exit.status",
    "exit.because",
    "instance",
    "worked_model",
]);
pub const STATUS: View = View::Detail(&[
    "control_plane",
    "control_plane_source",
    "organization",
    "organization_source",
    "workspaces",
    "agents",
    "triggers",
    "sessions",
    "integrations",
    "credentials",
    "profiles",
    "next",
]);
pub const UNRESOLVED: View = View::Detail(&[
    "control_plane",
    "control_plane_source",
    "organization",
    "organization_source",
    "organizations",
    "next",
]);

/// What a creation answers back is the identifier the next command is given.
pub const DECLARED: View = View::Value("id");
