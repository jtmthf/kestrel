//! Server-sent events down, POST up, plain HTTP (ADR-0002), specified by `openapi/link.json`
//! rather than shared as types with the supervisor that dials in over it.

pub mod credential;

use std::time::Duration;

use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{BoxError, Json, Router};
use futures_core::Stream;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::domain::{Exit, Run, RunId};
use crate::link::credential::Secret;
use crate::store::Store;
use crate::work;

pub const INSTRUCTIONS: &str = "/link/runs/{run}/instructions";
pub const REPORTS: &str = "/link/runs/{run}/reports";

/// Nothing subscribes to `Fanout` at 0.1 (ADR-0005), so a held-open stream learns of a new
/// instruction by asking `Store` again rather than by being told.
const POLL: Duration = Duration::from_millis(100);
const KEEP_ALIVE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Instruction {
    Start,
    Stop,
}

impl Instruction {
    pub const fn kind(&self) -> &'static str {
        match self {
            Instruction::Start => "start",
            Instruction::Stop => "stop",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentInstruction {
    pub seq: i64,
    pub instruction: Instruction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Report {
    Connected { version: String },
    Started,
    Finished { exit: Exit },
}

#[derive(Clone)]
struct ControlPlane {
    store: Store,
    shutdown: CancellationToken,
}

struct Waiting {
    instructions: Vec<SentInstruction>,
    the_run_ended: bool,
}

pub fn router(store: Store, shutdown: CancellationToken) -> Router {
    Router::new()
        .route(INSTRUCTIONS, get(instructions))
        .route(REPORTS, post(report))
        .with_state(ControlPlane { store, shutdown })
}

pub async fn instruct(
    store: &Store,
    run: &Run,
    instruction: Instruction,
) -> Result<SentInstruction> {
    let mut tx = store.begin().await?;
    let sent = tx.send_instruction(run, instruction).await?;
    tx.commit().await?;

    Ok(sent)
}

async fn instructions(
    State(control_plane): State<ControlPlane>,
    Path(run): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, BoxError>>>, Refused> {
    let run = authenticated(&control_plane, &headers, &run).await?;
    let mut cursor = cursor(&headers);
    info!(run = %run.id, cursor, "an environment is on the link");

    let stream = async_stream::try_stream! {
        loop {
            let waiting = waiting(&control_plane.store, run.id, cursor).await?;
            for sent in waiting.instructions {
                cursor = sent.seq;
                yield Event::default()
                    .id(sent.seq.to_string())
                    .event(sent.instruction.kind())
                    .json_data(&sent.instruction)?;
            }

            if waiting.the_run_ended {
                break;
            }

            tokio::select! {
                () = tokio::time::sleep(POLL) => {}
                () = control_plane.shutdown.cancelled() => break,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEP_ALIVE)))
}

async fn report(
    State(control_plane): State<ControlPlane>,
    Path(run): Path<String>,
    headers: HeaderMap,
    Json(report): Json<Report>,
) -> Result<StatusCode, Refused> {
    let run = authenticated(&control_plane, &headers, &run).await?;

    match report {
        Report::Connected { version } => {
            let mut tx = control_plane.store.begin().await?;
            tx.record_connected(&run, &version).await?;
            tx.commit().await?;
            info!(run = %run.id, version, "an environment reported itself connected");
        }
        Report::Started => {
            work::started(&control_plane.store, &run).await?;
            info!(run = %run.id, "an environment reported its run started");
        }
        Report::Finished { exit } => {
            match &exit {
                Exit::Succeeded => work::complete(&control_plane.store, &run).await?,
                Exit::Failed { because } => work::fail(&control_plane.store, &run, because).await?,
            }
            info!(run = %run.id, %exit, "an environment reported its run finished");
        }
    }

    Ok(StatusCode::ACCEPTED)
}

async fn waiting(store: &Store, run: RunId, cursor: i64) -> Result<Waiting> {
    let mut tx = store.begin().await?;
    let the_run_ended = tx.run(run).await?.ended_at.is_some();

    Ok(Waiting {
        instructions: tx.instructions_after(run, cursor).await?,
        the_run_ended,
    })
}

async fn authenticated(
    control_plane: &ControlPlane,
    headers: &HeaderMap,
    run: &str,
) -> Result<Run, Refused> {
    let run: RunId = run.parse().map_err(|_| Refused::NoSuchRun)?;
    let secret = bearer(headers).ok_or(Refused::Unauthorized("no credential presented"))?;

    let mut tx = control_plane.store.begin().await?;
    let credential = tx
        .credential(&secret.digest())
        .await?
        .ok_or(Refused::Unauthorized("no credential kestrel issued"))?;

    if !credential.is_live_at(Timestamp::now()) {
        return Err(Refused::Unauthorized("the credential is no longer live"));
    }
    if credential.run != run {
        return Err(Refused::Forbidden("the credential belongs to another run"));
    }

    Ok(tx.run(run).await?)
}

fn bearer(headers: &HeaderMap) -> Option<Secret> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(Secret::presented)
}

fn cursor(headers: &HeaderMap) -> i64 {
    headers
        .get("last-event-id")
        .and_then(|cursor| cursor.to_str().ok())
        .and_then(|cursor| cursor.parse().ok())
        .unwrap_or(0)
}

enum Refused {
    NoSuchRun,
    Unauthorized(&'static str),
    Forbidden(&'static str),
    Unavailable(anyhow::Error),
}

impl From<anyhow::Error> for Refused {
    fn from(error: anyhow::Error) -> Self {
        Refused::Unavailable(error)
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Refused::NoSuchRun => (StatusCode::NOT_FOUND, "no such run".to_owned()),
            Refused::Unauthorized(why) => (StatusCode::UNAUTHORIZED, why.to_owned()),
            Refused::Forbidden(why) => (StatusCode::FORBIDDEN, why.to_owned()),
            Refused::Unavailable(error) => {
                warn!(%error, "the link could not answer");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the link could not answer".to_owned(),
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
