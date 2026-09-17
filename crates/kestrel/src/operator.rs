//! The boundary a Client reaches the control plane over, specified by `openapi/operator.json`.
//! It authenticates nobody, so it is served apart from the link and on loopback (ADR-0015).

use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{BoxError, Json, Router};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::agent::{self, NotOffered};
use crate::domain::{Agent, Organization, SessionId, SessionState, Workspace};
use crate::log::{self, Cursor, Page, Unreadable, Window};
use crate::store::organization::NoSuchOrganization;
use crate::store::{Declared, Store};

pub const ORGANIZATIONS: &str = "/operator/organizations";
pub const WORKSPACES: &str = "/operator/organizations/{organization}/workspaces";
pub const AGENTS: &str = "/operator/organizations/{organization}/agents";
pub const TRANSCRIPT: &str = "/operator/sessions/{session}/transcript";

const NO_SUCH_SESSION: &str = "no such session";

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
        .route(WORKSPACES, get(workspaces).post(declare_workspace))
        .route(AGENTS, get(agents).post(declare_agent))
        .route(TRANSCRIPT, get(transcript))
        .with_state(ControlPlane { store, shutdown })
}

#[derive(Deserialize)]
struct OrganizationDeclaration {
    name: String,
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

#[derive(Serialize)]
struct OrganizationRecord {
    id: String,
    name: String,
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

impl From<Organization> for OrganizationRecord {
    fn from(organization: Organization) -> Self {
        Self {
            id: organization.id.to_string(),
            name: organization.name,
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
    let declared = tx.organizations().declare(&declaration.name).await?;
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
    Path(session): Path<String>,
    Query(following): Query<Following>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, BoxError>>>, Refused> {
    let session: SessionId = session
        .parse()
        .map_err(|_| Refused::NotFound(NO_SUCH_SESSION.to_owned()))?;
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
        .ok_or_else(|| Refused::NotFound(NO_SUCH_SESSION.to_owned()))?;
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
    Unprocessable(String),
    Unavailable(anyhow::Error),
}

impl Refused {
    fn into_error(self) -> BoxError {
        match self {
            Refused::BadRequest(why) | Refused::NotFound(why) | Refused::Unprocessable(why) => {
                why.into()
            }
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
        Refused::Unavailable(error)
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
