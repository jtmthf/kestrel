//! The boundary a Client reaches the control plane over, specified by `openapi/operator.json`.
//! It authenticates nobody, so it is served apart from the link and on loopback (ADR-0015).

use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{BoxError, Json, Router};
use futures_core::Stream;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::agent::{self, NotOffered};
use crate::cron::Cron;
use crate::declaration;
use crate::declined::Declined;
use crate::domain::{
    self, Agent, Connection, CorrelationMiss, Direction, EventRecordId, EventRefusal, Fires,
    Firing, Integration, Occurrence, Organization, Run, Schedule, Session, SessionId, SessionState,
    SubscriptionProfile, Templates, Trigger, Workspace,
};
use crate::filter::Filter;
use crate::integration::github::{self, Github};
use crate::integration::{self, Connecting, Registration};
use crate::log::{self, Cursor, Page, Unreadable, Window};
use crate::profile::{self, Entry};
use crate::provider::{self, Held};
use crate::store::organization::NoSuchOrganization;
use crate::store::{Declared, Store};
use crate::template::Template;
use crate::trigger::{self, apply};
use crate::{instance, session, start, work};

pub const ORGANIZATIONS: &str = "/operator/organizations";
pub const STARTS: &str = "/operator/starts";
pub const WORKSPACES: &str = "/operator/organizations/{organization}/workspaces";
pub const AGENTS: &str = "/operator/organizations/{organization}/agents";
pub const AGENT_MODEL: &str = "/operator/organizations/{organization}/agents/{agent}/model";
pub const DECLARATION: &str = "/operator/organizations/{organization}/declaration";
pub const DECLARATION_PREVIEW: &str = "/operator/organizations/{organization}/declaration/preview";
pub const CREDENTIALS: &str = "/operator/organizations/{organization}/credentials";
pub const CREDENTIAL: &str = "/operator/organizations/{organization}/credentials/{variable}";
pub const PROFILES: &str = "/operator/organizations/{organization}/profiles";
pub const PROFILE_VARIABLE: &str =
    "/operator/organizations/{organization}/profiles/{profile}/variables/{variable}";
/// One segment, with the path's slashes percent-encoded in it.
pub const PROFILE_FILE: &str =
    "/operator/organizations/{organization}/profiles/{profile}/files/{path}";
pub const INTEGRATIONS: &str = "/operator/organizations/{organization}/integrations";
pub const EVENT_REFUSAL: &str =
    "/operator/organizations/{organization}/integrations/{integration}/event-refusal";
pub const EVENTS: &str = "/operator/organizations/{organization}/events";
pub const EVENT: &str = "/operator/events/{record}";
pub const TRIGGERS: &str = "/operator/organizations/{organization}/triggers";
pub const TRIGGER: &str = "/operator/organizations/{organization}/triggers/{trigger}";
pub const TRIGGER_TEST: &str = "/operator/organizations/{organization}/triggers/{trigger}/test";
pub const TRIGGER_DISABLE: &str =
    "/operator/organizations/{organization}/triggers/{trigger}/disable";
pub const TRIGGER_ENABLE: &str = "/operator/organizations/{organization}/triggers/{trigger}/enable";
pub const TRIGGER_DISPATCH: &str =
    "/operator/organizations/{organization}/triggers/{trigger}/dispatch";
pub const APPLIED_TRIGGERS: &str = "/operator/organizations/{organization}/applied-triggers";
pub const APPLIED_TRIGGERS_PREVIEW: &str =
    "/operator/organizations/{organization}/applied-triggers/preview";
pub const INSTANCES: &str = "/operator/organizations/{organization}/instances";
pub const SESSIONS: &str = "/operator/organizations/{organization}/sessions";
pub const SESSION: &str = "/operator/organizations/{organization}/sessions/{session}";
pub const SESSION_MESSAGES: &str =
    "/operator/organizations/{organization}/sessions/{session}/messages";
pub const SESSION_SEAL: &str = "/operator/organizations/{organization}/sessions/{session}/seal";
pub const SESSION_INSTANCE_RELEASE: &str =
    "/operator/organizations/{organization}/sessions/{session}/instance/release";
pub const RUNS: &str = "/operator/organizations/{organization}/sessions/{session}/runs";
pub const RUN: &str = "/operator/organizations/{organization}/runs/{run}";
pub const RUN_STOP: &str = "/operator/organizations/{organization}/runs/{run}/stop";
pub const TRANSCRIPT: &str = "/operator/organizations/{organization}/sessions/{session}/transcript";

const EVENTS_LISTED: usize = 50;

const POLL: Duration = Duration::from_millis(100);
const KEEP_ALIVE: Duration = Duration::from_secs(15);

#[derive(Clone)]
struct ControlPlane {
    store: Store,
    shutdown: CancellationToken,
}

#[derive(Deserialize)]
struct Following {
    follow: Option<bool>,
}

#[derive(Serialize)]
struct Recorded {
    seq: i64,
    appended_at: String,
    entry: log::Entry,
}

#[derive(Serialize)]
struct End {
    because: Because,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Because {
    CaughtUp,
    Sealed,
}

struct Read {
    page: Page,
    sealed: bool,
}

pub fn router(store: Store, shutdown: CancellationToken) -> Router {
    Router::new()
        .route(ORGANIZATIONS, get(organizations).post(declare_organization))
        .route(STARTS, post(start))
        .route(WORKSPACES, get(workspaces).post(declare_workspace))
        .route(AGENTS, get(agents).post(declare_agent))
        .route(AGENT_MODEL, put(set_agent_model))
        .route(DECLARATION, post(apply_declaration))
        .route(DECLARATION_PREVIEW, post(preview_declaration))
        .route(CREDENTIALS, get(credentials))
        .route(CREDENTIAL, put(hold_credential).delete(forget_credential))
        .route(PROFILES, get(profiles).post(declare_profile))
        .route(
            PROFILE_VARIABLE,
            put(hold_profile_variable).delete(forget_profile_variable),
        )
        .route(
            PROFILE_FILE,
            put(hold_profile_file).delete(forget_profile_file),
        )
        .route(INTEGRATIONS, get(integrations).post(register_integration))
        .route(EVENT_REFUSAL, delete(acknowledge_event_refusal))
        .route(EVENTS, get(events))
        .route(EVENT, get(event))
        .route(TRIGGERS, get(triggers).post(declare_trigger))
        .route(TRIGGER, get(show_trigger))
        .route(TRIGGER_TEST, post(test_trigger))
        .route(TRIGGER_DISABLE, post(disable_trigger))
        .route(TRIGGER_ENABLE, post(enable_trigger))
        .route(TRIGGER_DISPATCH, post(dispatch_trigger))
        .route(APPLIED_TRIGGERS, post(apply_triggers))
        .route(APPLIED_TRIGGERS_PREVIEW, post(preview_applied_triggers))
        .route(INSTANCES, get(instances))
        .route(SESSIONS, get(sessions).post(open_session))
        .route(SESSION, get(show_session))
        .route(SESSION_MESSAGES, post(post_to_session))
        .route(SESSION_SEAL, post(seal_session))
        .route(SESSION_INSTANCE_RELEASE, post(release_instance))
        .route(RUNS, get(runs).post(enqueue_run))
        .route(RUN, get(show_run))
        .route(RUN_STOP, post(stop_run))
        .route(TRANSCRIPT, get(transcript))
        .with_state(ControlPlane { store, shutdown })
}

#[derive(Deserialize)]
struct OrganizationDeclaration {
    name: String,
    max_live_instances: Option<std::num::NonZeroUsize>,
}

#[derive(Deserialize)]
struct WorkspaceDeclaration {
    name: String,
    repositories: Vec<String>,
    branch: String,
}

#[derive(Deserialize)]
struct AgentDeclaration {
    name: String,
    runtime: String,
    model: Option<String>,
}

#[derive(Deserialize)]
struct SessionDeclaration {
    workspace: String,
    agent: String,
    profile: Option<String>,
    branch: Option<String>,
    continues: Option<String>,
}

#[derive(Deserialize)]
struct SessionMessage {
    #[serde(default = "default_operator_participant")]
    participant: String,
    message: String,
}

#[derive(Deserialize)]
struct RunDeclaration {
    model: Option<String>,
}

#[derive(Deserialize)]
struct TriggerDeclaration {
    name: String,
    filter: Option<serde_json::Value>,
    every: Option<String>,
    cron: Option<String>,
    zone: Option<String>,
    brief: String,
    branch: Option<String>,
    correlation: Option<String>,
    on_miss: Option<String>,
    workspace: String,
    agent: String,
    #[serde(default)]
    allows: Vec<String>,
    profile: Option<String>,
}

#[derive(Deserialize)]
struct TriggerTest {
    event: Option<String>,
    integration: Option<String>,
    issue: Option<i64>,
    instruction: Option<String>,
    agent: Option<String>,
    declared: Option<apply::File>,
}

#[derive(Deserialize)]
struct TriggerDispatch {
    integration: String,
    issue: i64,
    instruction: Option<String>,
    agent: Option<String>,
}

#[derive(Deserialize)]
struct AgentModel {
    model: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    #[serde(default = "default_operator_participant")]
    participant: String,
}

fn default_operator_participant() -> String {
    "operator".to_owned()
}

#[derive(Serialize)]
struct OrganizationRecord {
    id: String,
    name: String,
    max_live_instances: Option<std::num::NonZeroUsize>,
}

#[derive(Serialize)]
struct WorkspaceRecord {
    id: String,
    name: String,
    repositories: Vec<String>,
    branch: String,
}

#[derive(Serialize)]
struct AgentRecord {
    id: String,
    name: String,
    runtime: String,
    model: Option<String>,
}

#[derive(Serialize)]
struct SessionRecord {
    id: String,
    name: String,
    organization: String,
    workspace: String,
    agent: String,
    profile: Option<String>,
    checkout: domain::Checkout,
    instance: Option<String>,
    held: Option<String>,
    correlation: Option<String>,
    state: String,
    opened_at: Timestamp,
    last_active_at: Timestamp,
    sealed_at: Option<Timestamp>,
    continues: Option<String>,
    started_by: Option<String>,
    continued_by: Vec<String>,
}

#[derive(Serialize)]
struct RunRecord {
    id: String,
    name: String,
    session: String,
    state: String,
    waiting_for: Option<String>,
    exit: Option<domain::Exit>,
    outcome_message: Option<String>,
    instance: Option<String>,
    supervisor: Option<String>,
    model: Option<String>,
    worked_model: Option<String>,
    enqueued_at: Timestamp,
    started_at: Option<Timestamp>,
    ended_at: Option<Timestamp>,
    lease_expires_at: Option<Timestamp>,
    connected_at: Option<Timestamp>,
    supervisor_version: Option<String>,
    usage: Option<domain::Usage>,
}

#[derive(Serialize)]
struct TriggerRecord {
    id: String,
    organization: String,
    name: String,
    state: String,
    disabled_because: Option<String>,
    firing_budget: FiringBudgetRecord,
    filter: Option<serde_json::Value>,
    every: Option<String>,
    cron: Option<String>,
    zone: Option<String>,
    admits_outsiders: bool,
    brief: String,
    branch: Option<String>,
    correlation: Option<String>,
    on_miss: Option<String>,
    workspace: String,
    agent: String,
    allows: Vec<String>,
    profile: Option<String>,
    applied: bool,
    declared_at: Timestamp,
}

#[derive(Serialize)]
struct FiringBudgetRecord {
    limit: usize,
    window: String,
}

#[derive(Serialize)]
struct TriggerTestRecord {
    matches: bool,
    brief: String,
    branch: Option<String>,
    correlation: Option<String>,
    agent: String,
    elapsing: Option<Timestamp>,
}

impl From<Organization> for OrganizationRecord {
    fn from(organization: Organization) -> Self {
        Self {
            id: organization.id.to_string(),
            name: organization.name,
            max_live_instances: organization.max_live_instances,
        }
    }
}

impl From<Workspace> for WorkspaceRecord {
    fn from(workspace: Workspace) -> Self {
        Self {
            id: workspace.id.to_string(),
            name: workspace.name,
            repositories: workspace.repositories,
            branch: workspace.branch,
        }
    }
}

impl From<Agent> for AgentRecord {
    fn from(agent: Agent) -> Self {
        Self {
            id: agent.id.to_string(),
            name: agent.name,
            runtime: agent.runtime,
            model: agent.model,
        }
    }
}

impl SessionRecord {
    async fn read(store: &Store, session: Session) -> Result<Self, Refused> {
        let continued_by = session::continuations(store, session.id)
            .await?
            .into_iter()
            .map(|session| session.to_string())
            .collect();
        let instance = work::instance(store, session.id).await?;
        let held = instance::held_by(store, session.id)
            .await?
            .map(|held| held.because);

        Ok(Self {
            id: session.id.to_string(),
            name: session.name,
            organization: session.organization.name,
            workspace: session.workspace.name,
            agent: session.agent.name,
            profile: session.profile.map(|profile| profile.name),
            checkout: session.checkout,
            instance,
            held,
            correlation: session.correlation,
            state: session.state.as_str().to_owned(),
            opened_at: session.opened_at,
            last_active_at: session.last_active_at,
            sealed_at: session.sealed_at,
            continues: session.continues.map(|session| session.to_string()),
            started_by: session.started_by.map(|event| event.to_string()),
            continued_by,
        })
    }
}

impl RunRecord {
    fn read(run: Run) -> Self {
        Self {
            id: run.id.to_string(),
            name: run.name,
            session: run.session.to_string(),
            state: run.state.as_str().to_owned(),
            waiting_for: run.waiting_for,
            exit: run.exit,
            outcome_message: run.outcome_message,
            instance: run.instance,
            supervisor: run.supervisor,
            model: run.model,
            worked_model: run.worked_model,
            enqueued_at: run.enqueued_at,
            started_at: run.started_at,
            ended_at: run.ended_at,
            lease_expires_at: run.lease_expires_at,
            connected_at: run.connected.as_ref().map(|connected| connected.at),
            supervisor_version: run.connected.map(|connected| connected.version),
            usage: run.usage,
        }
    }

    fn all(runs: Vec<Run>) -> Vec<Self> {
        runs.into_iter().map(Self::read).collect()
    }
}

impl From<Trigger> for TriggerRecord {
    fn from(trigger: Trigger) -> Self {
        let (filter, every, cron, zone, admits_outsiders) = match trigger.fires {
            Fires::On(filter) => {
                let admits_outsiders = filter.admits_outsiders();
                (Some(filter.to_json()), None, None, None, admits_outsiders)
            }
            Fires::Scheduled(Schedule::Every(every)) => {
                (None, Some(format!("{every:#}")), None, None, false)
            }
            Fires::Scheduled(Schedule::Cron(cron)) => (
                None,
                None,
                Some(cron.to_string()),
                Some(cron.zone().to_owned()),
                false,
            ),
        };

        Self {
            id: trigger.id.to_string(),
            organization: trigger.organization.name,
            name: trigger.name,
            state: trigger.state.as_str().to_owned(),
            disabled_because: trigger.disabled_because,
            firing_budget: FiringBudgetRecord {
                limit: trigger.firing_budget.limit.get(),
                window: format!("{:#}", trigger.firing_budget.window),
            },
            filter,
            every,
            cron,
            zone,
            admits_outsiders,
            brief: trigger.templates.brief.to_string(),
            branch: trigger.templates.branch.map(|branch| branch.to_string()),
            correlation: trigger
                .templates
                .correlation
                .map(|correlation| correlation.to_string()),
            on_miss: trigger.on_miss.map(|miss| miss.as_str().to_owned()),
            workspace: trigger.workspace.name,
            agent: trigger.agent.name,
            allows: trigger.allows.into_iter().map(|agent| agent.name).collect(),
            profile: trigger.profile.map(|profile| profile.name),
            applied: trigger.applied,
            declared_at: trigger.declared_at,
        }
    }
}

#[derive(Serialize)]
struct StartedRecord {
    organization: start::Settled,
    workspace: start::Settled,
    agent: start::Settled,
    session: SessionRecord,
    run: RunRecord,
}

async fn start(
    State(control_plane): State<ControlPlane>,
    plan: Result<Json<start::Plan>, JsonRejection>,
) -> Result<(StatusCode, Json<StartedRecord>), Refused> {
    let Json(plan) = plan?;
    let started = start::start(&control_plane.store, &plan).await?;

    Ok((
        StatusCode::CREATED,
        Json(StartedRecord {
            organization: started.organization,
            workspace: started.workspace,
            agent: started.agent,
            session: SessionRecord::read(&control_plane.store, started.session).await?,
            run: RunRecord::read(started.run),
        }),
    ))
}

async fn organizations(
    State(control_plane): State<ControlPlane>,
) -> Result<Json<Vec<OrganizationRecord>>, Refused> {
    let mut tx = control_plane.store.begin().await?;
    let organizations = tx.organizations().all().await?;

    Ok(Json(organizations.into_iter().map(Into::into).collect()))
}

async fn declare_organization(
    State(control_plane): State<ControlPlane>,
    declaration: Result<Json<OrganizationDeclaration>, JsonRejection>,
) -> Result<Response, Refused> {
    let Json(declaration) = declaration?;
    named(&declaration.name)?;

    let mut tx = control_plane.store.begin().await?;
    let declared = tx
        .organizations()
        .declare(&declaration.name, declaration.max_live_instances)
        .await?;
    tx.commit().await?;

    Ok(answered::<_, OrganizationRecord>(declared))
}

async fn workspaces(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
) -> Result<Json<Vec<WorkspaceRecord>>, Refused> {
    let mut tx = control_plane.store.begin().await?;
    let organization = tx.organizations().named(&organization).await?;
    let workspaces = tx.workspaces().all(&organization).await?;

    Ok(Json(workspaces.into_iter().map(Into::into).collect()))
}

async fn declare_workspace(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    declaration: Result<Json<WorkspaceDeclaration>, JsonRejection>,
) -> Result<Response, Refused> {
    let Json(declaration) = declaration?;
    named(&declaration.name)?;
    if declaration.repositories.is_empty() {
        return Err(Refused::Unprocessable(
            "a workspace names at least one repository".to_owned(),
        ));
    }
    if declaration.branch.is_empty() {
        return Err(Refused::Unprocessable(
            "a workspace names the branch its work happens on".to_owned(),
        ));
    }
    if let Some(clash) = sharing_a_directory(&declaration.repositories) {
        return Err(Refused::Unprocessable(clash));
    }

    let mut tx = control_plane.store.begin().await?;
    let organization = tx.organizations().named(&organization).await?;
    let declared = tx
        .workspaces()
        .declare(
            &organization,
            &declaration.name,
            &declaration.repositories,
            &declaration.branch,
        )
        .await?;
    tx.commit().await?;

    Ok(answered::<_, WorkspaceRecord>(declared))
}

async fn agents(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
) -> Result<Json<Vec<AgentRecord>>, Refused> {
    let agents = agent::agents(&control_plane.store, &organization).await?;

    Ok(Json(agents.into_iter().map(Into::into).collect()))
}

async fn set_agent_model(
    State(control_plane): State<ControlPlane>,
    Path((organization, name)): Path<(String, String)>,
    model: Result<Json<AgentModel>, JsonRejection>,
) -> Result<Json<AgentRecord>, Refused> {
    let Json(model) = model?;
    let agent = agent::set_model(
        &control_plane.store,
        &organization,
        &name,
        model.model.as_deref(),
    )
    .await
    .map_err(named_refusal)?;

    Ok(Json(agent.into()))
}

async fn declare_agent(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    declaration: Result<Json<AgentDeclaration>, JsonRejection>,
) -> Result<Response, Refused> {
    let Json(declaration) = declaration?;
    named(&declaration.name)?;
    if declaration.runtime.is_empty() {
        return Err(Refused::Unprocessable(
            "an agent names the agent runtime that drives it".to_owned(),
        ));
    }

    let declared = agent::declare(
        &control_plane.store,
        &organization,
        &declaration.name,
        &declaration.runtime,
        declaration.model.as_deref(),
    )
    .await?;

    Ok(answered::<_, AgentRecord>(declared))
}

async fn apply_declaration(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    declaration: Result<Json<declaration::Document>, JsonRejection>,
) -> Result<Json<declaration::Applied>, Refused> {
    declared_declaration(
        control_plane,
        organization,
        declaration,
        declaration::ApplyMode::Apply,
    )
    .await
}

async fn preview_declaration(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    declaration: Result<Json<declaration::Document>, JsonRejection>,
) -> Result<Json<declaration::Applied>, Refused> {
    declared_declaration(
        control_plane,
        organization,
        declaration,
        declaration::ApplyMode::Preview,
    )
    .await
}

async fn declared_declaration(
    control_plane: ControlPlane,
    organization: String,
    declaration: Result<Json<declaration::Document>, JsonRejection>,
    mode: declaration::ApplyMode,
) -> Result<Json<declaration::Applied>, Refused> {
    let Json(declaration) = declaration?;
    let applied = declaration::apply(&control_plane.store, &organization, &declaration, mode)
        .await
        .map_err(declaration_refusal)?;

    Ok(Json(applied))
}

#[derive(Deserialize)]
struct Secret {
    secret: String,
}

#[derive(Serialize)]
struct CredentialRecord {
    variable: String,
    set_at: Timestamp,
}

impl From<Held> for CredentialRecord {
    fn from(held: Held) -> Self {
        Self {
            variable: held.variable,
            set_at: held.set_at,
        }
    }
}

async fn credentials(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
) -> Result<Json<Vec<CredentialRecord>>, Refused> {
    let held = provider::held(&control_plane.store, &organization).await?;

    Ok(Json(held.into_iter().map(Into::into).collect()))
}

async fn hold_credential(
    State(control_plane): State<ControlPlane>,
    Path((organization, variable)): Path<(String, String)>,
    secret: Result<Json<Secret>, JsonRejection>,
) -> Result<Json<CredentialRecord>, Refused> {
    let Json(Secret { secret }) = secret?;
    let held = provider::hold(&control_plane.store, &organization, &variable, &secret).await?;

    Ok(Json(held.into()))
}

async fn forget_credential(
    State(control_plane): State<ControlPlane>,
    Path((organization, variable)): Path<(String, String)>,
) -> Result<StatusCode, Refused> {
    provider::forget(&control_plane.store, &organization, &variable).await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ProfileDeclaration {
    name: String,
    owner: String,
}

#[derive(Serialize)]
struct ProfileRecord {
    id: String,
    name: String,
    owner: String,
}

#[derive(Serialize)]
struct ListedProfile {
    #[serde(flatten)]
    profile: ProfileRecord,
    holds: Vec<LoginRecord>,
}

#[derive(Serialize)]
struct LoginRecord {
    kind: &'static str,
    name: String,
    set_at: Timestamp,
}

impl From<SubscriptionProfile> for ProfileRecord {
    fn from(profile: SubscriptionProfile) -> Self {
        Self {
            id: profile.id.to_string(),
            name: profile.name,
            owner: profile.owner,
        }
    }
}

impl From<profile::Held> for LoginRecord {
    fn from(held: profile::Held) -> Self {
        Self {
            kind: held.entry.kind.as_str(),
            name: held.entry.name,
            set_at: held.set_at,
        }
    }
}

async fn profiles(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
) -> Result<Json<Vec<ListedProfile>>, Refused> {
    let listed = profile::profiles(&control_plane.store, &organization).await?;

    Ok(Json(
        listed
            .into_iter()
            .map(|(profile, held)| ListedProfile {
                profile: profile.into(),
                holds: held.into_iter().map(Into::into).collect(),
            })
            .collect(),
    ))
}

async fn declare_profile(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    declaration: Result<Json<ProfileDeclaration>, JsonRejection>,
) -> Result<Response, Refused> {
    let Json(declaration) = declaration?;
    named(&declaration.name)?;

    let declared = profile::declare(
        &control_plane.store,
        &organization,
        &declaration.name,
        &declaration.owner,
    )
    .await?;

    Ok(answered::<_, ProfileRecord>(declared))
}

async fn hold_profile_variable(
    State(control_plane): State<ControlPlane>,
    Path((organization, profile, variable)): Path<(String, String, String)>,
    secret: Result<Json<Secret>, JsonRejection>,
) -> Result<Json<LoginRecord>, Refused> {
    let entry = Entry::variable(&variable)?;
    held_in_profile(&control_plane, &organization, &profile, &entry, secret).await
}

async fn hold_profile_file(
    State(control_plane): State<ControlPlane>,
    Path((organization, profile, path)): Path<(String, String, String)>,
    secret: Result<Json<Secret>, JsonRejection>,
) -> Result<Json<LoginRecord>, Refused> {
    let entry = Entry::file(&path)?;
    held_in_profile(&control_plane, &organization, &profile, &entry, secret).await
}

async fn held_in_profile(
    control_plane: &ControlPlane,
    organization: &str,
    profile: &str,
    entry: &Entry,
    secret: Result<Json<Secret>, JsonRejection>,
) -> Result<Json<LoginRecord>, Refused> {
    let Json(Secret { secret }) = secret?;
    let held = profile::hold(&control_plane.store, organization, profile, entry, &secret).await?;

    Ok(Json(held.into()))
}

async fn forget_profile_variable(
    State(control_plane): State<ControlPlane>,
    Path((organization, profile, variable)): Path<(String, String, String)>,
) -> Result<StatusCode, Refused> {
    let entry = Entry::variable(&variable)?;
    profile::forget(&control_plane.store, &organization, &profile, &entry).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn forget_profile_file(
    State(control_plane): State<ControlPlane>,
    Path((organization, profile, path)): Path<(String, String, String)>,
) -> Result<StatusCode, Refused> {
    let entry = Entry::file(&path)?;
    profile::forget(&control_plane.store, &organization, &profile, &entry).await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct IntegrationRegistration {
    name: String,
    carries: Option<Vec<Direction>>,
    #[serde(flatten)]
    connection: ConnectionRegistration,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ConnectionRegistration {
    Github {
        repository: String,
        token: String,
        interval: Option<String>,
        webhook_secret: Option<String>,
        api: Option<String>,
    },
    Webhook {
        secret: String,
    },
}

/// Never the token or a webhook secret: what an Integration presents stays behind the boundary.
#[derive(Serialize)]
struct IntegrationRecord {
    id: String,
    name: String,
    kind: &'static str,
    repository: Option<String>,
    carries: Vec<Direction>,
    polled_every: Option<String>,
    webhook_path: Option<String>,
    last_event_refusal: Option<EventRefusalRecord>,
}

#[derive(Serialize)]
struct EventRefusalRecord {
    source: String,
    id: String,
    bytes: usize,
    reason: String,
    observed_at: Timestamp,
}

impl From<Integration> for IntegrationRecord {
    fn from(integration: Integration) -> Self {
        let webhook_path = integration.webhook_path();
        let kind = integration.kind().as_str();
        let (repository, polled_every, webhook_path) = match integration.connection {
            Connection::Github(github) if github.signed => {
                (Some(github.repository), None, Some(webhook_path))
            }
            Connection::Github(github) if !integration.carries.contains(&Direction::Inbound) => {
                (Some(github.repository), None, None)
            }
            Connection::Github(github) => (
                Some(github.repository),
                Some(format!("{:#}", github.interval)),
                None,
            ),
            Connection::Webhook => (None, None, Some(webhook_path)),
        };

        Self {
            id: integration.id.to_string(),
            name: integration.name,
            kind,
            repository,
            carries: integration.carries,
            polled_every,
            webhook_path,
            last_event_refusal: integration.last_event_refusal.map(Into::into),
        }
    }
}

impl From<EventRefusal> for EventRefusalRecord {
    fn from(refusal: EventRefusal) -> Self {
        Self {
            source: refusal.source,
            id: refusal.id,
            bytes: refusal.bytes,
            reason: refusal.reason,
            observed_at: refusal.observed_at,
        }
    }
}

async fn integrations(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
) -> Result<Json<Vec<IntegrationRecord>>, Refused> {
    let integrations = integration::integrations(&control_plane.store, &organization).await?;

    Ok(Json(integrations.into_iter().map(Into::into).collect()))
}

async fn register_integration(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    registration: Result<Json<IntegrationRegistration>, JsonRejection>,
) -> Result<(StatusCode, Json<IntegrationRecord>), Refused> {
    let Json(registration) = registration?;
    named(&registration.name)?;

    let (connecting, carries) = match &registration.connection {
        ConnectionRegistration::Github {
            repository,
            token,
            interval,
            webhook_secret,
            api,
        } => (
            Connecting::Github {
                repository,
                api: api.as_deref().unwrap_or(github::API),
                token,
                interval: interval
                    .as_deref()
                    .map_or(Ok(SignedDuration::from_mins(1)), str::parse)
                    .map_err(|error| {
                        Refused::Unprocessable(format!("an interval is a duration: {error}"))
                    })?,
                signing_secret: webhook_secret.as_deref(),
            },
            &[Direction::Inbound, Direction::Outbound][..],
        ),
        ConnectionRegistration::Webhook { secret } => {
            (Connecting::Webhook { secret }, &[Direction::Inbound][..])
        }
    };
    let registered = integration::register(
        &control_plane.store,
        Registration {
            organization: &organization,
            name: &registration.name,
            carries: registration.carries.as_deref().unwrap_or(carries),
            connecting,
        },
    )
    .await?;

    Ok((StatusCode::CREATED, Json(registered.into())))
}

async fn acknowledge_event_refusal(
    State(control_plane): State<ControlPlane>,
    Path((organization, integration)): Path<(String, String)>,
) -> Result<StatusCode, Refused> {
    integration::acknowledge_event_refusal(&control_plane.store, &organization, &integration)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Limited {
    limit: Option<usize>,
}

#[derive(Serialize)]
struct EventRecord {
    record: String,
    organization: String,
    integration: Option<String>,
    recorded_at: Timestamp,
    event: Occurrence,
    firings: Vec<Firing>,
}

impl EventRecord {
    async fn read(store: &Store, event: domain::Event) -> Result<Self, Refused> {
        Ok(Self {
            record: event.record_id.to_string(),
            organization: event.organization.to_string(),
            integration: event.integration.map(|integration| integration.to_string()),
            recorded_at: event.recorded_at,
            firings: trigger::firings(store, event.record_id).await?,
            event: event.occurrence,
        })
    }
}

async fn events(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    Query(limited): Query<Limited>,
) -> Result<Json<Vec<EventRecord>>, Refused> {
    let events = integration::events(
        &control_plane.store,
        &organization,
        limited.limit.unwrap_or(EVENTS_LISTED),
    )
    .await?;
    let mut records = Vec::new();
    for event in events {
        records.push(EventRecord::read(&control_plane.store, event).await?);
    }

    Ok(Json(records))
}

async fn event(
    State(control_plane): State<ControlPlane>,
    Path(record): Path<String>,
) -> Result<Json<EventRecord>, Refused> {
    let record: EventRecordId = record
        .parse()
        .map_err(|_| Refused::NotFound(format!("no event {record}")))?;
    let event = integration::event(&control_plane.store, record).await?;

    Ok(Json(EventRecord::read(&control_plane.store, event).await?))
}

async fn triggers(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
) -> Result<Json<Vec<TriggerRecord>>, Refused> {
    let triggers = trigger::triggers(&control_plane.store, &organization)
        .await
        .map_err(named_refusal)?;

    Ok(Json(triggers.into_iter().map(Into::into).collect()))
}

async fn declare_trigger(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    declaration: Result<Json<TriggerDeclaration>, JsonRejection>,
) -> Result<(StatusCode, Json<TriggerRecord>), Refused> {
    let Json(declaration) = declaration?;
    named(&declaration.name)?;
    let (fires, templates, on_miss) = parse_trigger_declaration(&declaration)?;
    let created = !trigger::triggers(&control_plane.store, &organization)
        .await
        .map_err(named_refusal)?
        .iter()
        .any(|trigger| trigger.name == declaration.name);
    let trigger = trigger::declare(
        &control_plane.store,
        trigger::Declaration {
            organization: &organization,
            name: &declaration.name,
            fires: &fires,
            templates: &templates,
            on_miss,
            workspace: &declaration.workspace,
            agent: &declaration.agent,
            allows: &declaration.allows,
            profile: declaration.profile.as_deref(),
        },
    )
    .await
    .map_err(named_refusal)?;

    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(trigger.into()),
    ))
}

async fn show_trigger(
    State(control_plane): State<ControlPlane>,
    Path((organization, name)): Path<(String, String)>,
) -> Result<Json<TriggerRecord>, Refused> {
    let trigger = trigger::show(&control_plane.store, &organization, &name)
        .await
        .map_err(named_refusal)?;

    Ok(Json(trigger.into()))
}

async fn test_trigger(
    State(control_plane): State<ControlPlane>,
    Path((organization, name)): Path<(String, String)>,
    tested: Result<Json<TriggerTest>, JsonRejection>,
) -> Result<Json<TriggerTestRecord>, Refused> {
    let Json(tested) = tested?;
    let event = tested
        .event
        .as_deref()
        .map(|event| {
            event
                .parse()
                .map_err(|_| Refused::NotFound(format!("no event {event}")))
        })
        .transpose()?;
    let github;
    let against = match (event, tested.integration.as_deref(), tested.issue) {
        (Some(_), _, Some(_)) => {
            return Err(Refused::BadRequest(
                "a test renders against an event or an issue, not both".to_owned(),
            ));
        }
        (Some(event), _, None) => trigger::Against::Event(event),
        (None, Some(integration), Some(issue)) => {
            github = Github::dialling_out()?;
            trigger::Against::Issue {
                github: &github,
                integration,
                issue,
            }
        }
        (None, None, Some(_)) => {
            return Err(Refused::BadRequest(
                "an issue is read through an integration, so a test naming one names both"
                    .to_owned(),
            ));
        }
        (None, Some(_), None) => {
            return Err(Refused::BadRequest(
                "an integration is read for an issue, so a test naming one names both".to_owned(),
            ));
        }
        (None, None, None) => trigger::Against::NextElapsing,
    };
    let asked = trigger::Asked {
        instruction: tested.instruction.as_deref(),
        agent: tested.agent.as_deref(),
    };
    let tested = match tested.declared {
        Some(file) => {
            let declarations = apply::declarations(file)
                .map_err(|error| Refused::Unprocessable(format!("{error:#}")))?;
            let declared = declarations
                .iter()
                .find(|declared| declared.name == name)
                .ok_or_else(|| {
                    Refused::Unprocessable(format!(
                        "the declaration file declares no trigger {name}"
                    ))
                })?;
            trigger::test_declared(
                &control_plane.store,
                &organization,
                declared,
                against,
                asked,
            )
            .await
        }
        None => trigger::test(&control_plane.store, &organization, &name, against, asked).await,
    }
    .map_err(integration_refusal)?;
    let rendered = tested
        .rendered
        .map_err(|error| Refused::Unprocessable(error.to_string()))?;
    let agent = tested
        .agent
        .map_err(|error| Refused::Unprocessable(error.to_string()))?;

    Ok(Json(TriggerTestRecord {
        matches: tested.matches,
        brief: rendered.brief,
        branch: rendered.branch,
        correlation: rendered.correlation,
        agent,
        elapsing: tested.elapsing,
    }))
}

async fn disable_trigger(
    State(control_plane): State<ControlPlane>,
    Path((organization, name)): Path<(String, String)>,
) -> Result<Json<TriggerRecord>, Refused> {
    let trigger = trigger::disable(&control_plane.store, &organization, &name)
        .await
        .map_err(named_refusal)?;

    Ok(Json(trigger.into()))
}

async fn enable_trigger(
    State(control_plane): State<ControlPlane>,
    Path((organization, name)): Path<(String, String)>,
) -> Result<Json<TriggerRecord>, Refused> {
    let trigger = trigger::enable(&control_plane.store, &organization, &name)
        .await
        .map_err(named_refusal)?;

    Ok(Json(trigger.into()))
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum FiredRecord {
    Opened {
        event: String,
        session: String,
        run: String,
    },
    Fed {
        event: String,
        session: String,
        run: Option<String>,
    },
    Ignored {
        event: String,
        correlation: String,
    },
}

async fn dispatch_trigger(
    State(control_plane): State<ControlPlane>,
    Path((organization, name)): Path<(String, String)>,
    dispatch: Result<Json<TriggerDispatch>, JsonRejection>,
) -> Result<Json<FiredRecord>, Refused> {
    let Json(dispatch) = dispatch?;
    let fired = trigger::dispatch(
        &control_plane.store,
        &Github::dialling_out()?,
        trigger::Dispatch {
            organization: &organization,
            trigger: &name,
            integration: &dispatch.integration,
            issue: dispatch.issue,
            asked: trigger::Asked {
                instruction: dispatch.instruction.as_deref(),
                agent: dispatch.agent.as_deref(),
            },
        },
    )
    .await
    .map_err(integration_refusal)?;

    Ok(Json(match fired {
        trigger::Fired::Opened {
            event,
            session,
            run,
        } => FiredRecord::Opened {
            event: event.to_string(),
            session: session.to_string(),
            run: run.to_string(),
        },
        trigger::Fired::Fed {
            event,
            session,
            run,
        } => FiredRecord::Fed {
            event: event.to_string(),
            session: session.to_string(),
            run: run.map(|run| run.to_string()),
        },
        trigger::Fired::Ignored {
            event, correlation, ..
        } => FiredRecord::Ignored {
            event: event.to_string(),
            correlation,
        },
        trigger::Fired::Failed { because, .. }
        | trigger::Fired::Held { because, .. }
        | trigger::Fired::Canceled { because, .. } => {
            return Err(Refused::Unprocessable(because));
        }
    }))
}

async fn apply_triggers(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    file: Result<Json<apply::File>, JsonRejection>,
) -> Result<Json<apply::Applied>, Refused> {
    applied_triggers(control_plane, organization, file, false).await
}

async fn preview_applied_triggers(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    file: Result<Json<apply::File>, JsonRejection>,
) -> Result<Json<apply::Applied>, Refused> {
    applied_triggers(control_plane, organization, file, true).await
}

async fn applied_triggers(
    control_plane: ControlPlane,
    organization: String,
    file: Result<Json<apply::File>, JsonRejection>,
    dry_run: bool,
) -> Result<Json<apply::Applied>, Refused> {
    let Json(file) = file?;
    let declarations =
        apply::declarations(file).map_err(|error| Refused::Unprocessable(format!("{error:#}")))?;
    let applied = apply::apply(&control_plane.store, &organization, &declarations, dry_run)
        .await
        .map_err(named_refusal)?;

    Ok(Json(applied))
}

fn parse_trigger_declaration(
    declaration: &TriggerDeclaration,
) -> Result<(Fires, Templates, Option<CorrelationMiss>), Refused> {
    let fires = match (
        &declaration.filter,
        &declaration.every,
        &declaration.cron,
        &declaration.zone,
    ) {
        (Some(filter), None, None, None) => Fires::On(
            Filter::from_json(filter)
                .map_err(|error| Refused::Unprocessable(format!("a trigger filter: {error}")))?,
        ),
        (None, Some(every), None, None) => {
            Fires::Scheduled(Schedule::Every(every.parse().map_err(|error| {
                Refused::Unprocessable(format!("a trigger schedule is a duration: {error}"))
            })?))
        }
        (None, None, Some(cron), Some(zone)) => {
            Fires::Scheduled(Schedule::Cron(Cron::new(cron, zone).map_err(|error| {
                Refused::Unprocessable(format!("a trigger schedule: {error:#}"))
            })?))
        }
        (None, None, Some(_), None) => {
            return Err(Refused::Unprocessable(
                "a cron expression declares the time zone it is read in".to_owned(),
            ));
        }
        (_, _, None, Some(_)) => {
            return Err(Refused::Unprocessable(
                "a time zone is declared only with a cron expression".to_owned(),
            ));
        }
        _ => {
            return Err(Refused::Unprocessable(
                "a trigger declares a filter, an interval or a cron expression, and only one"
                    .to_owned(),
            ));
        }
    };
    let templates = Templates {
        brief: declaration
            .brief
            .parse::<Template>()
            .map_err(|error| Refused::Unprocessable(format!("a trigger brief: {error}")))?,
        branch: declaration
            .branch
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(|error| Refused::Unprocessable(format!("a trigger branch: {error}")))?,
        correlation: declaration
            .correlation
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(|error| Refused::Unprocessable(format!("a trigger correlation: {error}")))?,
    };
    let on_miss = declaration
        .on_miss
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(|error| Refused::Unprocessable(format!("a trigger on_miss: {error}")))?;
    trigger::check_miss(&templates, on_miss)
        .map_err(|error| Refused::Unprocessable(error.to_string()))?;

    Ok((fires, templates, on_miss))
}

fn named_refusal(error: anyhow::Error) -> Refused {
    let message = error.to_string();
    if message.starts_with("no organization ")
        || message.starts_with("no workspace named ")
        || message.starts_with("no agent named ")
        || message.starts_with("no trigger named ")
        || message.starts_with("no event ")
    {
        return Refused::NotFound(message);
    }

    Refused::Unprocessable(message)
}

fn integration_refusal(error: anyhow::Error) -> Refused {
    match error.to_string() {
        missing if missing.starts_with("no integration named ") => Refused::NotFound(missing),
        _ => named_refusal(error),
    }
}

fn declaration_refusal(error: anyhow::Error) -> Refused {
    let message = error.to_string();
    if message.starts_with("no organization ") {
        return Refused::NotFound(message);
    }

    Refused::Unprocessable(message)
}

#[derive(Serialize)]
struct HeldRecord {
    session: String,
    instance: String,
    because: String,
}

async fn instances(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
) -> Result<Json<Vec<HeldRecord>>, Refused> {
    let held = instance::held(&control_plane.store, &organization).await?;

    Ok(Json(
        held.into_iter()
            .map(|held| HeldRecord {
                session: held.session.to_string(),
                instance: held.instance,
                because: held.because,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
struct ReleasedRecord {
    instance: String,
}

async fn release_instance(
    State(control_plane): State<ControlPlane>,
    Path((organization, session)): Path<(String, String)>,
    release: Result<Json<Release>, JsonRejection>,
) -> Result<Json<ReleasedRecord>, Refused> {
    let Json(release) = release?;
    let session = resolved(&control_plane, &organization, &session).await?;
    let instance = instance::release(&control_plane.store, session.id, &release.participant)
        .await
        .map_err(|error| match error.to_string() {
            none if none.ends_with("has no instance to release") => Refused::NotFound(none),
            _ => session_refusal(error),
        })?;

    Ok(Json(ReleasedRecord { instance }))
}

async fn sessions(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
) -> Result<Json<Vec<SessionRecord>>, Refused> {
    let sessions = session::sessions(&control_plane.store, &organization).await?;
    let mut records = Vec::with_capacity(sessions.len());
    for session in sessions {
        records.push(SessionRecord::read(&control_plane.store, session).await?);
    }

    Ok(Json(records))
}

async fn open_session(
    State(control_plane): State<ControlPlane>,
    Path(organization): Path<String>,
    declaration: Result<Json<SessionDeclaration>, JsonRejection>,
) -> Result<(StatusCode, Json<SessionRecord>), Refused> {
    let Json(declaration) = declaration?;
    let session = session::open(
        &control_plane.store,
        &organization,
        &declaration.workspace,
        &declaration.agent,
        declaration.profile.as_deref(),
        declaration.branch.as_deref(),
        declaration.continues.as_deref(),
    )
    .await
    .map_err(session_refusal)?;

    Ok((
        StatusCode::CREATED,
        Json(SessionRecord::read(&control_plane.store, session).await?),
    ))
}

async fn show_session(
    State(control_plane): State<ControlPlane>,
    Path((organization, session)): Path<(String, String)>,
) -> Result<Json<SessionRecord>, Refused> {
    let session = resolved(&control_plane, &organization, &session).await?;

    Ok(Json(
        SessionRecord::read(&control_plane.store, session).await?,
    ))
}

async fn post_to_session(
    State(control_plane): State<ControlPlane>,
    Path((organization, session)): Path<(String, String)>,
    message: Result<Json<SessionMessage>, JsonRejection>,
) -> Result<Json<Option<RunRecord>>, Refused> {
    let Json(message) = message?;
    let session = resolved(&control_plane, &organization, &session).await?;
    let run = session::post(
        &control_plane.store,
        session.id,
        &message.participant,
        &message.message,
    )
    .await
    .map_err(session_refusal)?;
    let run = run.map(RunRecord::read);

    Ok(Json(run))
}

async fn seal_session(
    State(control_plane): State<ControlPlane>,
    Path((organization, session)): Path<(String, String)>,
) -> Result<Json<SessionRecord>, Refused> {
    let session = resolved(&control_plane, &organization, &session).await?;
    let session = session::seal(&control_plane.store, session.id)
        .await
        .map_err(session_refusal)?;

    Ok(Json(
        SessionRecord::read(&control_plane.store, session).await?,
    ))
}

async fn runs(
    State(control_plane): State<ControlPlane>,
    Path((organization, session)): Path<(String, String)>,
) -> Result<Json<Vec<RunRecord>>, Refused> {
    let session = resolved(&control_plane, &organization, &session).await?;
    let runs = work::runs(&control_plane.store, session.id)
        .await
        .map_err(session_refusal)?;

    Ok(Json(RunRecord::all(runs)))
}

async fn enqueue_run(
    State(control_plane): State<ControlPlane>,
    Path((organization, session)): Path<(String, String)>,
    declaration: Result<Json<RunDeclaration>, JsonRejection>,
) -> Result<(StatusCode, Json<RunRecord>), Refused> {
    let Json(declaration) = declaration?;
    let session = resolved(&control_plane, &organization, &session).await?;
    let run = work::enqueue(
        &control_plane.store,
        session.id,
        declaration.model.as_deref(),
    )
    .await
    .map_err(session_refusal)?;

    Ok((StatusCode::CREATED, Json(RunRecord::read(run))))
}

async fn show_run(
    State(control_plane): State<ControlPlane>,
    Path((organization, run)): Path<(String, String)>,
) -> Result<Json<RunRecord>, Refused> {
    let run = work::resolve_run(&control_plane.store, &organization, &run).await?;

    Ok(Json(RunRecord::read(run)))
}

async fn stop_run(
    State(control_plane): State<ControlPlane>,
    Path((organization, run)): Path<(String, String)>,
) -> Result<Json<RunRecord>, Refused> {
    let run = work::resolve_run(&control_plane.store, &organization, &run).await?;
    work::stop(&control_plane.store, run.id)
        .await
        .map_err(|error| match error.to_string() {
            ended if ended.ends_with("has already ended") => Refused::Conflict(ended),
            _ => error.into(),
        })?;
    let run = work::run(&control_plane.store, run.id).await?;

    Ok(Json(RunRecord::read(run)))
}

async fn resolved(
    control_plane: &ControlPlane,
    organization: &str,
    session: &str,
) -> Result<Session, Refused> {
    session::resolve(&control_plane.store, organization, session)
        .await
        .map_err(session_refusal)
}

fn session_refusal(error: anyhow::Error) -> Refused {
    let message = error.to_string();
    if message.starts_with("no session ")
        || message.starts_with("no workspace named ")
        || message.starts_with("no agent named ")
    {
        return Refused::NotFound(message);
    }
    if message.contains("already has the run")
        || message.contains("still in flight")
        || message.contains("already sealed")
    {
        return Refused::Conflict(message);
    }
    if message.contains("is sealed") || message.contains("is open, and work continues") {
        return Refused::Unprocessable(message);
    }

    error.into()
}

fn sharing_a_directory(repositories: &[String]) -> Option<String> {
    let mut claimed = std::collections::HashMap::new();
    repositories.iter().find_map(|repository| {
        let directory = cloned_into(repository);
        claimed.insert(directory, repository).map(|earlier| {
            format!("{earlier} and {repository} would both be checked out into {directory}")
        })
    })
}

// Must name the directory kestrel-supervisor's checkout clones into.
fn cloned_into(repository: &str) -> &str {
    let name = repository
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(repository);

    name.strip_suffix(".git").unwrap_or(name)
}

fn named(name: &str) -> Result<(), Refused> {
    if name.is_empty() {
        return Err(Refused::Unprocessable("a name cannot be empty".to_owned()));
    }
    Ok(())
}

fn answered<T, R>(declared: Declared<T>) -> Response
where
    R: From<T> + Serialize,
{
    let status = if declared.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };

    (status, Json(R::from(declared.record))).into_response()
}

/// A stream that closes without an `end` event was cut off, and the reader resumes it from
/// the last id it was handed.
async fn transcript(
    State(control_plane): State<ControlPlane>,
    Path((organization, session)): Path<(String, String)>,
    Query(following): Query<Following>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, BoxError>>>, Refused> {
    let session = resolved(&control_plane, &organization, &session).await?;
    let session = session.id;
    let follow = following.follow.unwrap_or(true);
    let from = last_event_id(&headers)?;
    let mut read = reading(&control_plane.store, session, from).await?;

    let stream = async_stream::try_stream! {
        loop {
            for entry in read.page.entries {
                yield Event::default()
                    .id(Cursor::at(session, entry.seq).to_string())
                    .event("entry")
                    .json_data(Recorded {
                        seq: entry.seq,
                        appended_at: entry.appended_at.to_string(),
                        entry: entry.entry,
                    })?;
            }
            if !read.page.more {
                let because = match (read.sealed, follow) {
                    (true, _) => Some(Because::Sealed),
                    (false, false) => Some(Because::CaughtUp),
                    (false, true) => None,
                };
                if let Some(because) = because {
                    yield Event::default().event("end").json_data(End { because })?;
                    break;
                }

                tokio::select! {
                    () = tokio::time::sleep(POLL) => {}
                    () = control_plane.shutdown.cancelled() => break,
                }
            }

            read = reading(&control_plane.store, session, read.page.cursor)
                .await
                .map_err(Refused::into_error)?;
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEP_ALIVE)))
}

/// The state is read in the transaction the page is, so a Session sealed between the two
/// cannot end the stream short of its last entry.
async fn reading(store: &Store, id: SessionId, from: Option<Cursor>) -> Result<Read, Refused> {
    let mut tx = store.begin().await?;
    let session = tx
        .sessions()
        .find(id)
        .await?
        .ok_or_else(|| Refused::NotFound("no session".to_owned()))?;
    let page = tx.log().page(&session, from, Window::DEFAULT).await?;

    Ok(Read {
        page,
        sealed: session.state == SessionState::Sealed,
    })
}

fn last_event_id(headers: &HeaderMap) -> Result<Option<Cursor>, Refused> {
    let Some(cursor) = headers.get("last-event-id") else {
        return Ok(None);
    };

    cursor
        .to_str()
        .map_err(|error| Refused::BadRequest(error.to_string()))?
        .parse()
        .map(Some)
        .map_err(|error: anyhow::Error| Refused::BadRequest(error.to_string()))
}

enum Refused {
    BadRequest(String),
    NotFound(String),
    Conflict(String),
    Unprocessable(String),
    Unavailable(anyhow::Error),
}

impl Refused {
    fn into_error(self) -> BoxError {
        match self {
            Refused::BadRequest(why)
            | Refused::NotFound(why)
            | Refused::Conflict(why)
            | Refused::Unprocessable(why) => why.into(),
            Refused::Unavailable(error) => error.into(),
        }
    }
}

impl From<anyhow::Error> for Refused {
    fn from(error: anyhow::Error) -> Self {
        if let Some(missing) = error.downcast_ref::<NoSuchOrganization>() {
            return Refused::NotFound(missing.to_string());
        }
        if let Some(refused) = error.downcast_ref::<NotOffered>() {
            return Refused::Unprocessable(refused.to_string());
        }
        match error.downcast::<Declined>() {
            Ok(Declined::Unacceptable(why)) => Refused::Unprocessable(why),
            Ok(Declined::Missing(why) | Declined::Ambiguous(why)) => Refused::NotFound(why),
            Ok(Declined::Taken(why)) => Refused::Conflict(why),
            Err(error) => Refused::Unavailable(error),
        }
    }
}

impl From<JsonRejection> for Refused {
    fn from(rejection: JsonRejection) -> Self {
        Refused::BadRequest(rejection.body_text())
    }
}

impl From<Unreadable> for Refused {
    fn from(unreadable: Unreadable) -> Self {
        match unreadable {
            Unreadable::Cursor(why) => Refused::BadRequest(why),
            Unreadable::Unavailable(error) => Refused::Unavailable(error),
        }
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Refused::BadRequest(why) => (StatusCode::BAD_REQUEST, why),
            Refused::NotFound(why) => (StatusCode::NOT_FOUND, why),
            Refused::Conflict(why) => (StatusCode::CONFLICT, why),
            Refused::Unprocessable(why) => (StatusCode::UNPROCESSABLE_ENTITY, why),
            Refused::Unavailable(error) => {
                warn!(%error, "the operator boundary could not answer");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the control plane could not answer".to_owned(),
                )
            }
        };

        (status, Json(Refusal { message })).into_response()
    }
}

#[derive(Serialize)]
struct Refusal {
    message: String,
}
