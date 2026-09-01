//! Server-sent events down, POST up, specified by `openapi/link.json` rather than shared as
//! types with the control plane that serves it.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Client, Response, StatusCode, header};
use serde::{Deserialize, Serialize};

pub const INSTRUCTIONS: &str = "/link/runs/{run}/instructions";
pub const REPORTS: &str = "/link/runs/{run}/reports";

/// A run reaches the link as one path segment, whatever the Environment was handed.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Instruction {
    Start,
    Stop,
    /// A control plane kestrel upgraded under a live Environment (ADR-0002) may send an
    /// instruction this supervisor predates; letting it past keeps the cursor moving.
    #[serde(other)]
    Unrecognized,
}

impl Instruction {
    pub const fn kind(&self) -> &'static str {
        match self {
            Instruction::Start => "start",
            Instruction::Stop => "stop",
            Instruction::Unrecognized => "unrecognized",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Report {
    Connected { version: String },
    Started,
    Said { message: String },
    Used { usage: Usage },
    Finished { exit: Exit },
}

/// How the work this Environment was provisioned for went. Everything the control plane makes
/// of it is the control plane's business.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Exit {
    Succeeded,
    Failed { because: String },
}

/// What the agent has spent so far, cumulative rather than per turn.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Usage {
    pub context_used: u64,
    pub context_size: u64,
    pub cost: Option<Cost>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    pub id: String,
    pub instruction: Instruction,
}

#[derive(Debug)]
pub enum Error {
    /// The link declined this Environment. Reconnecting with the same credential will not help.
    Refused(String),
    Lost(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Refused(why) | Error::Lost(why) => out.write_str(why),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Error::Lost(error.to_string())
    }
}

pub struct Link {
    client: Client,
    base: String,
    run: String,
    credential: String,
}

pub struct Instructions {
    response: Response,
    buffered: Vec<u8>,
}

impl Link {
    pub fn to(base: &str, run: &str, credential: &str) -> Self {
        Self {
            client: Client::new(),
            base: base.to_owned(),
            run: run.to_owned(),
            credential: credential.to_owned(),
        }
    }

    pub async fn report(&self, report: &Report) -> Result<(), Error> {
        let response = self
            .client
            .post(self.url(REPORTS))
            .bearer_auth(&self.credential)
            .json(report)
            .send()
            .await?;

        let response = refuse_if_declined(response).await?;
        if !response.status().is_success() {
            return Err(Error::Lost(format!(
                "the link answered {} to a report",
                response.status().as_u16()
            )));
        }

        Ok(())
    }

    pub async fn open(&self, cursor: Option<&str>) -> Result<Instructions, Error> {
        let mut request = self
            .client
            .get(self.url(INSTRUCTIONS))
            .bearer_auth(&self.credential)
            .header(header::ACCEPT, "text/event-stream");
        if let Some(cursor) = cursor {
            request = request.header("last-event-id", cursor);
        }

        let response = refuse_if_declined(request.send().await?).await?;
        if !response.status().is_success() {
            return Err(Error::Lost(format!(
                "the link answered {} to a stream",
                response.status().as_u16()
            )));
        }

        Ok(Instructions {
            response,
            buffered: Vec::new(),
        })
    }

    fn url(&self, path: &str) -> String {
        let run = utf8_percent_encode(&self.run, SEGMENT).to_string();

        format!("{}{}", self.base, path.replace("{run}", &run))
    }
}

impl Instructions {
    pub async fn next(&mut self) -> Result<Option<Delivered>, Error> {
        loop {
            if let Some((id, data)) = self.take_frame() {
                let instruction = serde_json::from_str(&data).map_err(|error| {
                    Error::Lost(format!(
                        "the stream carried {data}, which is not an instruction: {error}"
                    ))
                })?;

                return Ok(Some(Delivered { id, instruction }));
            }

            match self.response.chunk().await? {
                Some(chunk) => self.buffered.extend_from_slice(&chunk),
                None => return Ok(None),
            }
        }
    }

    /// A frame is everything up to a blank line; the keep-alive is a frame with only a comment.
    fn take_frame(&mut self) -> Option<(String, String)> {
        loop {
            let end = self.buffered.windows(2).position(|pair| pair == b"\n\n")?;
            let frame: Vec<u8> = self.buffered.drain(..end + 2).collect();
            let frame = String::from_utf8_lossy(&frame);

            let mut id = None;
            let mut data = String::new();
            for line in frame.lines() {
                if let Some(carried) = line.strip_prefix("id:") {
                    id = Some(carried.trim().to_owned());
                } else if let Some(carried) = line.strip_prefix("data:") {
                    data.push_str(carried.trim());
                }
            }

            if let Some(id) = id
                && !data.is_empty()
            {
                return Some((id, data));
            }
        }
    }
}

async fn refuse_if_declined(response: Response) -> Result<Response, Error> {
    match response.status() {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => {
            Err(Error::Refused(response.text().await?))
        }
        _ => Ok(response),
    }
}
