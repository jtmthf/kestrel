//! The boundary a Client reaches the control plane over, specified by `openapi/operator.json`.
//! It authenticates nobody, so it is served apart from the link and on loopback (ADR-0015).

use std::time::Duration;

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

use crate::domain::{SessionId, SessionState};
use crate::log::{self, Cursor, Page, Unreadable, Window};
use crate::store::Store;

pub const TRANSCRIPT: &str = "/operator/sessions/{session}/transcript";

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
        .route(TRANSCRIPT, get(transcript))
        .with_state(ControlPlane { store, shutdown })
}

/// A stream that closes without an `end` event was cut off, and the reader resumes it from
/// the last id it was handed.
async fn transcript(
    State(control_plane): State<ControlPlane>,
    Path(session): Path<String>,
    Query(following): Query<Following>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, BoxError>>>, Refused> {
    let session: SessionId = session.parse().map_err(|_| Refused::NoSuchSession)?;
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
        .ok_or(Refused::NoSuchSession)?;
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
    NoSuchSession,
    Unavailable(anyhow::Error),
}

impl Refused {
    fn into_error(self) -> BoxError {
        match self {
            Refused::BadRequest(why) => why.into(),
            Refused::NoSuchSession => "no such session".into(),
            Refused::Unavailable(error) => error.into(),
        }
    }
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
            Refused::NoSuchSession => (StatusCode::NOT_FOUND, "no such session".to_owned()),
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
