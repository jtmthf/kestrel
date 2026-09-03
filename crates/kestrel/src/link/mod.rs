//! Server-sent events down, POST up, plain HTTP (ADR-0002), specified by `openapi/link.json`
//! rather than shared as types with the supervisor that dials in over it.

pub mod credential;

use std::time::Duration;

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{BoxError, Json, Router};
use futures_core::Stream;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::domain::{Exit, Run, RunId, Usage};
use crate::link::credential::Secret;
use crate::log::{self, Cursor, Unreadable, Window};
use crate::session;
use crate::store::{Store, Taken};
use crate::work;

/// The Transcript of the Session the Run belongs to. Named for what crosses the link rather
/// than for what it is, because the supervisor is a courier and may not know (ADR-0002).
pub const ENTRIES: &str = "/link/runs/{run}/entries";
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Report {
    Connected { version: String },
    Heartbeat,
    Started,
    Said { message: String },
    Used { usage: Usage },
    Finished { exit: Exit },
}

impl Report {
    /// Whether this report changes the Run's durable record, and so is one the Environment
    /// numbers and the control plane takes once. What it does not number costs nothing to
    /// take twice.
    const fn numbered(&self) -> bool {
        match self {
            Report::Connected { .. } | Report::Heartbeat => false,
            Report::Started
            | Report::Said { .. }
            | Report::Used { .. }
            | Report::Finished { .. } => true,
        }
    }
}

/// A report as it reaches the link: the numbering is on the envelope rather than inside any
/// one kind of report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reported {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    #[serde(flatten)]
    pub report: Report,
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

#[derive(Deserialize)]
struct Paging {
    cursor: Option<String>,
    window: Option<usize>,
}

#[derive(Serialize)]
struct Entries {
    entries: Vec<Recorded>,
    cursor: Option<String>,
    more: bool,
}

#[derive(Serialize)]
struct Recorded {
    seq: i64,
    appended_at: String,
    entry: log::Entry,
}

pub fn router(store: Store, shutdown: CancellationToken) -> Router {
    Router::new()
        .route(ENTRIES, get(entries))
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
    let mut cursor = last_event_id(&headers);
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

async fn entries(
    State(control_plane): State<ControlPlane>,
    Path(run): Path<String>,
    Query(paging): Query<Paging>,
    headers: HeaderMap,
) -> Result<Json<Entries>, Refused> {
    let run = authenticated(&control_plane, &headers, &run).await?;
    let from = paging
        .cursor
        .as_deref()
        .map(str::parse::<Cursor>)
        .transpose()
        .map_err(|error| Refused::BadRequest(error.to_string()))?;
    let window = Window::or_default(paging.window)
        .map_err(|error| Refused::BadRequest(error.to_string()))?;

    let page = session::transcript(&control_plane.store, run.session, from, window).await?;

    Ok(Json(Entries {
        entries: page
            .entries
            .into_iter()
            .map(|entry| Recorded {
                seq: entry.seq,
                appended_at: entry.appended_at.to_string(),
                entry: entry.entry,
            })
            .collect(),
        cursor: page.cursor.map(|cursor| cursor.to_string()),
        more: page.more,
    }))
}

/// Everything a numbered report changes commits with the number that took it, so replaying
/// one whose answer never arrived changes the record once (ADR-0004).
async fn report(
    State(control_plane): State<ControlPlane>,
    Path(run): Path<String>,
    headers: HeaderMap,
    Json(Reported { seq, report }): Json<Reported>,
) -> Result<StatusCode, Refused> {
    let run = authenticated(&control_plane, &headers, &run).await?;
    let mut tx = control_plane.store.begin().await?;

    if report.numbered() {
        let seq = seq.ok_or(Refused::BadRequest(
            "this report changes the run's record, and carries no seq".to_owned(),
        ))?;
        match tx.take_report(&run, seq).await? {
            Taken::Next => {}
            Taken::Again => {
                debug!(run = %run.id, seq, "an environment reported something again");
                return Ok(StatusCode::ACCEPTED);
            }
            Taken::Skipped => {
                return Err(Refused::BadRequest(format!(
                    "the report {seq} skips one this run has yet to report"
                )));
            }
        }
    }

    match report {
        Report::Connected { version } => {
            tx.record_connected(&run, &version).await?;
            info!(run = %run.id, version, "an environment reported itself connected");
        }
        Report::Heartbeat => {
            work::heartbeat(&mut tx, &run).await?;
            debug!(run = %run.id, "an environment reported itself alive");
        }
        Report::Started => {
            work::started(&mut tx, &run).await?;
            info!(run = %run.id, "an environment reported its run started");
        }
        Report::Said { message } => {
            work::saying(&mut tx, &run, &message).await?;
            info!(run = %run.id, "an environment reported what its agent said");
        }
        Report::Used { usage } => {
            info!(run = %run.id, %usage, "an environment reported what its agent used");
            work::used(&mut tx, &run, &usage).await?;
        }
        Report::Finished { exit } => {
            let stands = work::ending(&mut tx, &run, exit).await?;
            info!(run = %run.id, %stands, "an environment reported its run finished");
        }
    }
    tx.commit().await?;

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

fn last_event_id(headers: &HeaderMap) -> i64 {
    headers
        .get("last-event-id")
        .and_then(|cursor| cursor.to_str().ok())
        .and_then(|cursor| cursor.parse().ok())
        .unwrap_or(0)
}

enum Refused {
    BadRequest(String),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_numbered_report_carries_its_seq_beside_its_kind() {
        let sent = serde_json::json!({"kind": "said", "seq": 3, "message": "what it said"});

        let reported: Reported = serde_json::from_value(sent.clone()).expect("a report");

        assert_eq!(reported.seq, Some(3));
        assert_eq!(
            reported.report,
            Report::Said {
                message: "what it said".to_owned()
            }
        );
        assert_eq!(serde_json::to_value(&reported).expect("a report"), sent);
    }

    #[test]
    fn a_report_the_environment_does_not_number_carries_no_seq() {
        let sent = serde_json::json!({"kind": "heartbeat"});

        let reported: Reported = serde_json::from_value(sent.clone()).expect("a report");

        assert_eq!(reported.seq, None);
        assert!(!reported.report.numbered());
        assert_eq!(serde_json::to_value(&reported).expect("a report"), sent);
    }
}
