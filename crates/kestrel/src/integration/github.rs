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
/// label, so a trigger matching on the label alone would fire on both.
pub const LABELLED: &str = "labeled";
pub const COMMENTED: &str = "commented";

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

            // What was polled through says how far back to walk, and nothing about what to
            // hand back: an Event already recorded is recognised by its identity, so a window
            // that overlaps the last one costs a recognition rather than a duplicate.
            let reached = reported
                .iter()
                .any(|event| Some(event.id) <= integration.polled_through);
            for event in reported {
                through = through.max(Some(event.id));
                if let Some(occurrence) = occurrence(event) {
                    newest_first.push(occurrence);
                }
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

        for page in 1..=PAGES {
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
            let reported: Vec<IssueComment> =
                answered(response, &format!("the comments on {repository}")).await?;
            let short = reported.len() < PER_PAGE;
            let reached = reported
                .iter()
                .any(|comment| Some(comment.id) <= integration.comments_polled_through);

            for comment in reported {
                through = through.max(Some(comment.id));
                if Some(comment.id) > integration.comments_polled_through
                    && !comment.body.contains("<!-- kestrel run ")
                    && let Some(occurrence) = comment.occurrence()
                {
                    newest_first.push(occurrence);
                }
            }

            if short || reached || integration.comments_polled_through.is_none() {
                break;
            }
            if page == PAGES {
                warn!(
                    integration = integration.name,
                    "github had more comments waiting than one poll reads"
                );
            }
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
    ) -> Result<Vec<IssueEvent>, Refused> {
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

fn occurrence(event: IssueEvent) -> Option<Occurrence> {
    let issue = event.issue?;

    Some(Occurrence {
        external_id: event.id.to_string(),
        kind: event.event,
        actor: event.actor.map(|actor| actor.login).unwrap_or_default(),
        subject: issue.number,
        title: issue.title,
        url: issue.html_url,
        label: event.label.map(|label| label.name),
        message: None,
        occurred_at: event.created_at.parse().ok()?,
    })
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

#[derive(Debug, Deserialize)]
struct IssueEvent {
    id: i64,
    event: String,
    created_at: String,
    actor: Option<Actor>,
    label: Option<Label>,
    issue: Option<Issue>,
}

#[derive(Debug, Deserialize)]
struct Actor {
    login: String,
}

#[derive(Debug, Deserialize)]
struct Label {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Issue {
    number: i64,
    title: String,
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct IssueComment {
    id: i64,
    body: String,
    created_at: String,
    html_url: String,
    issue_url: String,
    user: Option<Actor>,
}

impl IssueComment {
    fn occurrence(self) -> Option<Occurrence> {
        let subject = self.issue_url.rsplit('/').next()?.parse().ok()?;

        Some(Occurrence {
            external_id: format!("comment:{}", self.id),
            kind: COMMENTED.to_owned(),
            actor: self.user.map(|user| user.login).unwrap_or_default(),
            subject,
            title: String::new(),
            url: self.html_url,
            label: None,
            message: Some(self.body),
            occurred_at: self.created_at.parse().ok()?,
        })
    }
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
}
