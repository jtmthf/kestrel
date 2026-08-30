//! What a supervisor does, said one request at a time, so a refusal can be observed as a
//! status code rather than inferred from behaviour.

use std::time::Duration;

use kestrel::domain::RunId;
use kestrel::link::Report;
use kestrel::link::credential::Secret;
use reqwest::{Client, Response, StatusCode, header};

pub struct Link {
    client: Client,
    base: String,
}

pub enum Next {
    Event(Event),
    Quiet,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: Option<String>,
    pub name: Option<String>,
    pub data: String,
}

pub struct Events {
    response: Response,
    buffered: String,
}

impl Link {
    pub fn to(base: &str) -> Self {
        Self {
            client: Client::new(),
            base: base.to_owned(),
        }
    }

    pub async fn instructions(
        &self,
        run: RunId,
        credential: Option<&Secret>,
        cursor: Option<i64>,
    ) -> Response {
        self.instructions_for(&run.to_string(), credential, cursor)
            .await
    }

    pub async fn instructions_for(
        &self,
        run: &str,
        credential: Option<&Secret>,
        cursor: Option<i64>,
    ) -> Response {
        let mut request = self
            .client
            .get(format!("{}/link/runs/{run}/instructions", self.base));
        if let Some(credential) = credential {
            request = request.bearer_auth(credential.as_str());
        }
        if let Some(cursor) = cursor {
            request = request.header("Last-Event-ID", cursor.to_string());
        }

        request.send().await.expect("the link should answer")
    }

    pub async fn open(&self, run: RunId, credential: &Secret, cursor: Option<i64>) -> Events {
        let response = self.instructions(run, Some(credential), cursor).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the link refused to open the stream"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|kind| kind.to_str().ok()),
            Some("text/event-stream")
        );

        Events {
            response,
            buffered: String::new(),
        }
    }

    pub async fn report(
        &self,
        run: RunId,
        credential: Option<&Secret>,
        report: &Report,
    ) -> Response {
        let mut request = self
            .client
            .post(format!("{}/link/runs/{run}/reports", self.base))
            .json(report);
        if let Some(credential) = credential {
            request = request.bearer_auth(credential.as_str());
        }

        request.send().await.expect("the link should answer")
    }
}

impl Events {
    pub async fn next_within(&mut self, patience: Duration) -> Next {
        let deadline = tokio::time::Instant::now() + patience;

        loop {
            if let Some(event) = self.take_frame() {
                return Next::Event(event);
            }
            match tokio::time::timeout_at(deadline, self.response.chunk()).await {
                Ok(Ok(Some(chunk))) => self.buffered.push_str(&String::from_utf8_lossy(&chunk)),
                Ok(Ok(None)) => return Next::Closed,
                Ok(Err(error)) => panic!("the stream failed: {error}"),
                Err(_) => return Next::Quiet,
            }
        }
    }

    /// Comment-only frames are the keep-alive, and carry nothing to assert on.
    fn take_frame(&mut self) -> Option<Event> {
        loop {
            let end = self.buffered.find("\n\n")?;
            let frame: String = self.buffered.drain(..end + 2).collect();

            let mut event = Event {
                id: None,
                name: None,
                data: String::new(),
            };
            for line in frame.lines() {
                if let Some(id) = line.strip_prefix("id:") {
                    event.id = Some(id.trim().to_owned());
                } else if let Some(name) = line.strip_prefix("event:") {
                    event.name = Some(name.trim().to_owned());
                } else if let Some(data) = line.strip_prefix("data:") {
                    event.data.push_str(data.trim());
                }
            }

            if event.id.is_some() || event.name.is_some() || !event.data.is_empty() {
                return Some(event);
            }
        }
    }
}
