use std::fmt;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use jiff::Timestamp;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::domain::{Integration, Occurrence};

pub const API: &str = "https://api.github.com";

/// GitHub reports a label coming off an issue as an `unlabeled` event carrying that same
/// label, so a trigger matching on the type and label alone would fire on both.
pub const LABELLED: &str = "com.github.issues.labeled";
pub const COMMENTED: &str = "com.github.issue_comment.created";

const VERSION: &str = "2022-11-28";
const PER_PAGE: usize = 100;
const REQUEST: Duration = Duration::from_secs(30);

/// GitHub answers newest first, so a poll walks back until it reaches what the last one saw.
/// The walk is capped so that one poll is bounded: reaching the cap says more happened between
/// two polls than a poll reads, and it is the one shape in which an Event goes unread — so it
/// is said out loud rather than swallowed.
const PAGES: usize = 10;

/// Why a poll came back with nothing rather than with no events. Neither advances what the
/// Integration has been polled through, so the next poll covers the same window again.
#[derive(Debug)]
pub enum Refused {
    RateLimited { until: Timestamp },
    Failed(anyhow::Error),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::RateLimited { until } => write!(f, "github is rate limiting until {until}"),
            Refused::Failed(error) => write!(f, "{error}"),
        }
    }
}

pub struct Seen {
    pub occurrences: Vec<Occurrence>,
    pub through: Option<i64>,
}

pub struct Github {
    client: reqwest::Client,
}

impl Github {
    pub fn dialling_out() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(REQUEST)
                .user_agent(concat!("kestrel/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("building the client kestrel polls github with")?,
        })
    }

    /// Everything on the watched repository since the Integration was last polled through,
    /// oldest first. A first poll takes one page rather than the repository's whole history:
    /// an Integration discovers what happens from the moment it is registered.
    pub async fn issue_events(&self, integration: &Integration) -> Result<Seen, Refused> {
        let repository = repository(&integration.repository).map_err(Refused::Failed)?;
        let mut newest_first = Vec::new();
        let mut through = integration.polled_through;

        for page in 1..=PAGES {
            let reported = self.page(integration, &repository, page).await?;
            let short = reported.len() < PER_PAGE;
            let ids = reported
                .iter()
                .map(event_id)
                .collect::<Result<Vec<_>>>()
                .map_err(Refused::Failed)?;

            // What was polled through says how far back to walk, and nothing about what to
            // hand back: an Event already recorded is recognised by its identity, so a window
            // that overlaps the last one costs a recognition rather than a duplicate.
            let reached = reported
                .iter()
                .zip(&ids)
                .any(|(_, id)| Some(*id) <= integration.polled_through);
            for (event, id) in reported.into_iter().zip(ids) {
                through = through.max(Some(id));
                newest_first.push(occurrence(&event, integration).map_err(Refused::Failed)?);
            }

            if short || reached || integration.polled_through.is_none() {
                break;
            }
            if page == PAGES {
                warn!(
                    integration = integration.name,
                    "github had more events waiting than one poll reads"
                );
            }
        }

        newest_first.reverse();
        Ok(Seen {
            occurrences: newest_first,
            through,
        })
    }

    pub async fn issue_comments(&self, integration: &Integration) -> Result<Seen, Refused> {
        let repository = repository(&integration.repository).map_err(Refused::Failed)?;
        let mut newest_first = Vec::new();
        let mut through = integration.comments_polled_through;

        let mut page = 1;
        loop {
            let response = self
                .request(
                    reqwest::Method::GET,
                    integration,
                    &format!(
                        "repos/{repository}/issues/comments?sort=created&direction=desc&per_page={PER_PAGE}&page={page}"
                    ),
                )
                .send()
                .await
                .map_err(|error| {
                    Refused::Failed(anyhow!("the comments on {repository} could not be polled: {error}"))
                })?;
            let reported: Vec<serde_json::Value> =
                answered(response, &format!("the comments on {repository}")).await?;
            let short = reported.len() < PER_PAGE;
            let ids = reported
                .iter()
                .map(comment_id)
                .collect::<Result<Vec<_>>>()
                .map_err(Refused::Failed)?;
            let reached = ids
                .iter()
                .any(|id| Some(*id) <= integration.comments_polled_through);

            for (comment, id) in reported.into_iter().zip(ids) {
                through = through.max(Some(id));
                if Some(id) > integration.comments_polled_through
                    && !comment
                        .get("body")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|body| body.contains("<!-- kestrel run "))
                {
                    newest_first
                        .push(comment_occurrence(&comment, integration).map_err(Refused::Failed)?);
                }
            }

            if short || reached || integration.comments_polled_through.is_none() {
                break;
            }
            page += 1;
        }

        newest_first.reverse();
        Ok(Seen {
            occurrences: newest_first,
            through,
        })
    }

    async fn page(
        &self,
        integration: &Integration,
        repository: &str,
        page: usize,
    ) -> Result<Vec<serde_json::Value>, Refused> {
        let response = self
            .request(
                reqwest::Method::GET,
                integration,
                &format!("repos/{repository}/issues/events?per_page={PER_PAGE}&page={page}"),
            )
            .send()
            .await
            .map_err(|error| {
                Refused::Failed(anyhow!("{repository} could not be polled: {error}"))
            })?;

        answered(response, &format!("the events on {repository}")).await
    }

    /// The comment kestrel leaves on the issue the work came from. What comes back is where
    /// it landed, so a delivery that is asked again recognises its own comment.
    pub async fn comment(
        &self,
        integration: &Integration,
        subject: i64,
        body: &str,
    ) -> Result<Comment, Refused> {
        let repository = repository(&integration.repository).map_err(Refused::Failed)?;
        let response = self
            .request(
                reqwest::Method::POST,
                integration,
                &format!("repos/{repository}/issues/{subject}/comments"),
            )
            .json(&Body { body })
            .send()
            .await
            .map_err(|error| {
                Refused::Failed(anyhow!(
                    "{repository}#{subject} could not be commented on: {error}"
                ))
            })?;

        answered(response, &format!("a comment on {repository}#{subject}")).await
    }

    /// Whether a comment carrying `marker` is already on the issue. `since` bounds the read to
    /// the window an attempt could have landed in, so this is one page rather than a walk back
    /// through everything an issue has ever collected.
    pub async fn comment_carrying(
        &self,
        integration: &Integration,
        subject: i64,
        marker: &str,
        since: Timestamp,
    ) -> Result<Option<Comment>, Refused> {
        let repository = repository(&integration.repository).map_err(Refused::Failed)?;
        let response = self
            .request(
                reqwest::Method::GET,
                integration,
                &format!(
                    "repos/{repository}/issues/{subject}/comments?per_page={PER_PAGE}&since={since}"
                ),
            )
            .send()
            .await
            .map_err(|error| {
                Refused::Failed(anyhow!(
                    "the comments on {repository}#{subject} could not be read: {error}"
                ))
            })?;

        let comments: Vec<Comment> =
            answered(response, &format!("the comments on {repository}#{subject}")).await?;

        Ok(comments
            .into_iter()
            .find(|comment| comment.body.contains(marker)))
    }

    fn request(
        &self,
        method: reqwest::Method,
        integration: &Integration,
        path: &str,
    ) -> reqwest::RequestBuilder {
        self.client
            .request(
                method,
                format!("{}/{path}", integration.api.trim_end_matches('/')),
            )
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", VERSION)
            .bearer_auth(integration.credential.presented_to_the_external_system())
    }
}

async fn answered<T: DeserializeOwned>(response: Response, asked_for: &str) -> Result<T, Refused> {
    let status = response.status();
    if let Some(until) = rate_limited(status, response.headers()) {
        return Err(Refused::RateLimited { until });
    }
    if !status.is_success() {
        return Err(Refused::Failed(anyhow!(
            "github answered {status} for {asked_for}"
        )));
    }

    response.json().await.map_err(|error| {
        Refused::Failed(anyhow!(
            "github's account of {asked_for} could not be read: {error}"
        ))
    })
}

/// Both of the ways GitHub says to come back later: the reset moment on an exhausted quota,
/// and the seconds a secondary limit asks for.
fn rate_limited(status: StatusCode, headers: &HeaderMap) -> Option<Timestamp> {
    if status != StatusCode::FORBIDDEN && status != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }

    if let Some(seconds) = number(headers.get("retry-after")) {
        return Some(Timestamp::now() + jiff::SignedDuration::from_secs(seconds.max(0)));
    }
    if number(headers.get("x-ratelimit-remaining")) == Some(0) {
        return number(headers.get("x-ratelimit-reset"))
            .and_then(|reset| Timestamp::from_second(reset).ok());
    }

    None
}

fn number(header: Option<&HeaderValue>) -> Option<i64> {
    header?.to_str().ok()?.trim().parse().ok()
}

fn event_id(event: &serde_json::Value) -> Result<i64> {
    event
        .get("id")
        .and_then(serde_json::Value::as_i64)
        .context("a GitHub issue event has no integer id")
}

fn comment_id(comment: &serde_json::Value) -> Result<i64> {
    comment
        .get("id")
        .and_then(serde_json::Value::as_i64)
        .context("a GitHub issue comment has no integer id")
}

fn occurrence(event: &serde_json::Value, integration: &Integration) -> Result<Occurrence> {
    let issue = event
        .get("issue")
        .context("a GitHub issue event names no issue")?;
    let event_kind = event
        .get("event")
        .and_then(serde_json::Value::as_str)
        .context("a GitHub issue event has no type")?;

    Ok(Occurrence {
        id: event_id(event)?.to_string(),
        source: source(integration),
        specversion: "1.0".to_owned(),
        r#type: if event_kind == "labeled" {
            LABELLED.to_owned()
        } else {
            format!("com.github.issues.{event_kind}")
        },
        subject: Some(format!(
            "#{}",
            issue
                .get("number")
                .and_then(serde_json::Value::as_i64)
                .context("a GitHub issue event has no issue number")?
        )),
        time: event
            .get("created_at")
            .and_then(serde_json::Value::as_str)
            .context("a GitHub issue event has no time")?
            .parse()
            .context("a GitHub issue event has an invalid time")?,
        data: event.clone(),
    })
}

fn comment_occurrence(
    comment: &serde_json::Value,
    integration: &Integration,
) -> Result<Occurrence> {
    let issue = comment
        .get("issue_url")
        .and_then(serde_json::Value::as_str)
        .context("a GitHub issue comment names no issue")?
        .rsplit('/')
        .next()
        .context("a GitHub issue comment has an invalid issue URL")?
        .parse::<i64>()
        .context("a GitHub issue comment has an invalid issue number")?;

    Ok(Occurrence {
        id: format!("comment:{}", comment_id(comment)?),
        source: source(integration),
        specversion: "1.0".to_owned(),
        r#type: COMMENTED.to_owned(),
        subject: Some(format!("#{issue}")),
        time: comment
            .get("created_at")
            .and_then(serde_json::Value::as_str)
            .context("a GitHub issue comment has no time")?
            .parse()
            .context("a GitHub issue comment has an invalid time")?,
        data: comment.clone(),
    })
}

pub struct EventData<'a> {
    occurrence: &'a Occurrence,
}

impl<'a> EventData<'a> {
    pub fn new(occurrence: &'a Occurrence) -> Self {
        Self { occurrence }
    }

    pub fn actor(&self) -> Option<&str> {
        self.field(&["actor", "login"])
            .or_else(|| self.field(&["user", "login"]))
            .and_then(serde_json::Value::as_str)
    }

    pub fn label(&self) -> Option<&str> {
        self.field(&["label", "name"])
            .and_then(serde_json::Value::as_str)
    }

    pub fn title(&self) -> Option<&str> {
        self.field(&["issue", "title"])
            .and_then(serde_json::Value::as_str)
    }

    pub fn url(&self) -> Option<&str> {
        self.field(&["html_url"])
            .or_else(|| self.field(&["issue", "html_url"]))
            .and_then(serde_json::Value::as_str)
    }

    pub fn message(&self) -> Option<&str> {
        self.field(&["body"]).and_then(serde_json::Value::as_str)
    }

    pub fn subject_issue(&self) -> Option<i64> {
        self.occurrence
            .subject
            .as_deref()
            .and_then(|subject| subject.strip_prefix('#'))
            .and_then(|number| number.parse().ok())
    }

    fn field(&self, path: &[&str]) -> Option<&serde_json::Value> {
        let mut at = &self.occurrence.data;
        for part in path {
            at = at.get(*part)?;
        }
        Some(at)
    }
}

/// The external resource the event is about (ADR-0011): the repository, never the
/// integration, so an event dedups identically however kestrel learned it.
fn source(integration: &Integration) -> String {
    format!("https://github.com/{}", integration.repository)
}

/// `owner/name`, checked here because it is pasted into a URL rather than sent as a parameter.
pub fn repository(repository: &str) -> Result<String> {
    let (owner, name) = repository
        .split_once('/')
        .with_context(|| format!("{repository} is not a github repository: name it owner/name"))?;

    for part in [owner, name] {
        if part.is_empty()
            || !part
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
        {
            bail!("{repository} is not a github repository: name it owner/name");
        }
    }

    Ok(format!("{owner}/{name}"))
}

/// One comment as GitHub reports it, and what it answers a newly posted one with.
#[derive(Debug, Deserialize)]
pub struct Comment {
    pub html_url: String,
    #[serde(default)]
    body: String,
}

#[derive(Serialize)]
struct Body<'a> {
    body: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
                HeaderValue::from_str(value).expect("a header value"),
            );
        }

        headers
    }

    #[test]
    fn an_exhausted_quota_says_when_it_resets() {
        let until = rate_limited(
            StatusCode::FORBIDDEN,
            &headers(&[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1789000000"),
            ]),
        );

        assert_eq!(until, Timestamp::from_second(1_789_000_000).ok());
    }

    #[test]
    fn a_secondary_limit_asks_for_seconds_rather_than_a_moment() {
        let until = rate_limited(
            StatusCode::TOO_MANY_REQUESTS,
            &headers(&[("retry-after", "60")]),
        )
        .expect("a retry-after is a rate limit");

        assert!(until > Timestamp::now() + jiff::SignedDuration::from_secs(50));
    }

    #[test]
    fn a_forbidden_answer_with_quota_left_is_a_failure_rather_than_a_rate_limit() {
        assert_eq!(
            rate_limited(
                StatusCode::FORBIDDEN,
                &headers(&[("x-ratelimit-remaining", "4999")])
            ),
            None
        );
    }

    #[test]
    fn a_repository_is_owner_and_name() {
        assert_eq!(repository("jtmthf/kestrel").unwrap(), "jtmthf/kestrel");
        assert!(repository("kestrel").is_err());
        assert!(repository("jtmthf/kestrel/issues").is_err());
        assert!(repository("../../secrets").is_err());
        assert!(repository("jtmthf/").is_err());
    }

    #[test]
    fn an_issue_event_without_a_producer_id_is_refused() {
        let event = serde_json::json!({
            "event": "labeled",
            "created_at": "2026-09-01T12:00:00Z",
            "issue": { "number": 43 }
        });
        let integration = Integration {
            id: crate::domain::IntegrationId::generate(),
            organization: crate::domain::OrganizationId::generate(),
            name: "github".to_owned(),
            kind: crate::domain::IntegrationKind::Github,
            repository: "jtmthf/kestrel".to_owned(),
            api: API.to_owned(),
            credential: crate::integration::credential::Token::held("nothing"),
            carries: vec![crate::domain::Direction::Inbound],
            interval: jiff::SignedDuration::from_secs(60),
            poll_due_at: None,
            polled_through: None,
            comments_polled_through: None,
            last_event_refusal: None,
        };

        let refusal = occurrence(&event, &integration)
            .expect_err("an event without its producer id should be refused");

        assert!(refusal.to_string().contains("integer id"));
    }
}
