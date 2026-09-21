use std::io::Write as _;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use reqwest::{Client, Url, header};
use serde::Deserialize;
use serde_json::Value;

use crate::output::Presentation;
use crate::sse::Events;
use crate::view;

/// How long a stream may stay unreachable before the read gives up on it.
const PATIENCE: Duration = Duration::from_secs(30);
const RETRY: Duration = Duration::from_millis(250);

#[derive(Deserialize)]
struct Refusal {
    message: String,
}

enum Cut {
    Refused(String),
    Lost(anyhow::Error),
    Failed(anyhow::Error),
}

impl From<reqwest::Error> for Cut {
    fn from(error: reqwest::Error) -> Self {
        Cut::Lost(error.into())
    }
}

/// Reads until the control plane ends the stream on purpose, reconnecting from the last entry
/// printed whenever it is cut off. Returns the cursor the read ended at.
pub async fn read(
    control_plane: &Url,
    organization: &str,
    session: &str,
    from: Option<String>,
    follow: bool,
    presentation: &Presentation,
) -> Result<Option<String>> {
    let client = Client::new();
    let url = transcript(control_plane, organization, session, follow)?;
    let mut cursor = from;
    let mut heard = Instant::now();

    loop {
        match streamed(&client, &url, &mut cursor, &mut heard, presentation).await {
            Ok(()) => return Ok(cursor),
            Err(Cut::Refused(why)) => bail!("the control plane refused the read: {why}"),
            Err(Cut::Failed(error)) => return Err(error),
            Err(Cut::Lost(error)) if heard.elapsed() > PATIENCE => {
                return Err(error.context(format!("reading the transcript from {control_plane}")));
            }
            Err(Cut::Lost(_)) => tokio::time::sleep(RETRY).await,
        }
    }
}

async fn streamed(
    client: &Client,
    url: &Url,
    cursor: &mut Option<String>,
    heard: &mut Instant,
    presentation: &Presentation,
) -> Result<(), Cut> {
    let mut request = client
        .get(url.clone())
        .header(header::ACCEPT, "text/event-stream");
    if let Some(cursor) = cursor {
        request = request.header("last-event-id", cursor.as_str());
    }

    let response = request.send().await?;
    let status = response.status();
    if status.is_client_error() {
        let why = response
            .json::<Refusal>()
            .await
            .map_or_else(|_| status.to_string(), |refusal| refusal.message);
        return Err(Cut::Refused(why));
    }
    if !status.is_success() {
        return Err(Cut::Lost(anyhow!("the control plane answered {status}")));
    }
    *heard = Instant::now();

    let mut events = Events::over(response);
    let mut stdout = std::io::stdout().lock();
    while let Some(event) = events.next().await? {
        *heard = Instant::now();
        match event.name.as_deref() {
            Some("entry") => {
                let entry = presented(&event.data, presentation).map_err(Cut::Failed)?;
                writeln!(stdout, "{entry}")
                    .and_then(|()| stdout.flush())
                    .map_err(|error| {
                        Cut::Failed(anyhow!(error).context("writing the transcript"))
                    })?;
                *cursor = event.id;
            }
            Some("end") => return Ok(()),
            _ => {}
        }
    }

    Err(Cut::Lost(anyhow!("the stream closed before it ended")))
}

fn presented(data: &str, presentation: &Presentation) -> Result<String> {
    let entry: Value = serde_json::from_str(data).context("reading a transcript entry")?;

    crate::output::line(presentation, &view::ENTRIES, &entry)
}

fn transcript(control_plane: &Url, organization: &str, session: &str, follow: bool) -> Result<Url> {
    let mut url = control_plane.clone();
    url.path_segments_mut()
        .map_err(|()| anyhow!("{control_plane} cannot be a base for a path"))
        .context("addressing the transcript")?
        .pop_if_empty()
        .extend([
            "operator",
            "organizations",
            organization,
            "sessions",
            session,
            "transcript",
        ]);
    url.query_pairs_mut()
        .append_pair("follow", if follow { "true" } else { "false" });

    Ok(url)
}
