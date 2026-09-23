use std::fmt;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use jiff::Timestamp;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::declined::Declined;
use crate::domain::{GithubConnection, Integration, Occurrence};
use crate::readiness::{Delegation, Readiness, WorkState};

pub const API: &str = "https://api.github.com";

/// GitHub reports a label coming off an issue as an `unlabeled` event carrying that same
/// label, so a trigger matching on the type and label alone would fire on both.
pub const LABELLED: &str = "com.github.issues.labeled";
pub const COMMENTED: &str = "com.github.issue_comment.created";

/// A comment that opens with it is a command to kestrel rather than a remark to the Session.
pub const MENTION: &str = "@kestrel";
const AGENT: &str = "agent=";

/// Every outcome comment carries it, so kestrel never hears its own comment as a follow-up.
pub const MARKER: &str = "<!-- kestrel run ";

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
        let github = integration.github().map_err(Refused::Failed)?;
        let repository = repository(&github.repository).map_err(Refused::Failed)?;
        let mut newest_first = Vec::new();
        let mut through = integration.polled_through;

        for page in 1..=PAGES {
            let reported = self.page(github, &repository, page).await?;
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
                newest_first.push(occurrence(&event, github).map_err(Refused::Failed)?);
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
        let github = integration.github().map_err(Refused::Failed)?;
        let repository = repository(&github.repository).map_err(Refused::Failed)?;
        let mut newest_first = Vec::new();
        let mut through = integration.comments_polled_through;

        let mut page = 1;
        loop {
            let response = self
                .request(
                    reqwest::Method::GET,
                    github,
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
                if Some(id) > integration.comments_polled_through && !said_by_kestrel(&comment) {
                    newest_first
                        .push(comment_occurrence(&comment, github).map_err(Refused::Failed)?);
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
        github: &GithubConnection,
        repository: &str,
        page: usize,
    ) -> Result<Vec<serde_json::Value>, Refused> {
        let response = self
            .request(
                reqwest::Method::GET,
                github,
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
        let github = integration.github().map_err(Refused::Failed)?;
        let repository = repository(&github.repository).map_err(Refused::Failed)?;
        let response = self
            .request(
                reqwest::Method::POST,
                github,
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
        let github = integration.github().map_err(Refused::Failed)?;
        let repository = repository(&github.repository).map_err(Refused::Failed)?;
        let response = self
            .request(
                reqwest::Method::GET,
                github,
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

    pub async fn issue(
        &self,
        integration: &Integration,
        number: i64,
    ) -> Result<serde_json::Value, Refused> {
        let github = integration.github().map_err(Refused::Failed)?;
        let repository = repository(&github.repository).map_err(Refused::Failed)?;
        let response = self
            .request(
                reqwest::Method::GET,
                github,
                &format!("repos/{repository}/issues/{number}"),
            )
            .send()
            .await
            .map_err(|error| {
                Refused::Failed(anyhow!("{repository}#{number} could not be read: {error}"))
            })?;

        answered(response, &format!("{repository}#{number}")).await
    }

    pub async fn readiness(
        &self,
        integration: &Integration,
        occurrence: &Occurrence,
    ) -> Result<Readiness, Refused> {
        let github = integration.github().map_err(Refused::Failed)?;
        let number = EventData::new(occurrence)
            .subject_issue()
            .ok_or_else(|| Refused::Failed(anyhow!("the event names no GitHub issue")))?;
        let work_item = format!("https://github.com/{}/issues/{number}", github.repository);
        let issue = self.issue(integration, number).await?;
        if issue.get("number").and_then(serde_json::Value::as_i64) != Some(number) {
            return Err(Refused::Failed(anyhow!(
                "github returned a different issue for {work_item}"
            )));
        }
        let state = match issue.get("state").and_then(serde_json::Value::as_str) {
            Some("open") => WorkState::Open,
            Some("closed") => WorkState::Terminal,
            _ => WorkState::Unknown,
        };
        let assigned = issue
            .get("assignees")
            .and_then(serde_json::Value::as_array)
            .map(|assignees| {
                assignees.iter().any(|assignee| {
                    assignee
                        .get("login")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|login| {
                            login.eq_ignore_ascii_case(MENTION.trim_start_matches('@'))
                        })
                })
            });

        let mut evidence = vec![work_item.clone()];
        let command = EventData::new(occurrence).command();
        let live_command = if command.is_some() && assigned != Some(true) {
            let id = occurrence
                .id
                .strip_prefix("comment:")
                .and_then(|id| id.parse::<i64>().ok());
            match id {
                Some(id) => {
                    let response = self
                        .request(
                            reqwest::Method::GET,
                            github,
                            &format!(
                                "repos/{}/issues/comments/{id}",
                                repository(&github.repository).map_err(Refused::Failed)?
                            ),
                        )
                        .send()
                        .await
                        .map_err(|error| {
                            Refused::Failed(anyhow!(
                                "the command on {work_item} could not be read: {error}"
                            ))
                        })?;
                    let comment: serde_json::Value =
                        answered(response, &format!("the command on {work_item}")).await?;
                    evidence.push(format!("{work_item}#issuecomment-{id}"));
                    let original = EventData::new(occurrence);
                    let actor = original.actor();
                    let mut current = occurrence.clone();
                    current.data = comment;
                    let current_actor = current
                        .data
                        .get("user")
                        .and_then(|user| user.get("login"))
                        .and_then(serde_json::Value::as_str);
                    let same_issue = current
                        .data
                        .get("issue_url")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|url| url.ends_with(&format!("/issues/{number}")));
                    Some(
                        actor
                            .zip(current_actor)
                            .is_some_and(|(before, now)| before.eq_ignore_ascii_case(now))
                            && same_issue
                            && EventData::new(&current).command() == command,
                    )
                }
                None => None,
            }
        } else {
            Some(false)
        };
        let delegation = match (assigned, live_command) {
            (Some(true), _) | (_, Some(true)) => Delegation::Current,
            (Some(false), Some(false)) => Delegation::Absent,
            _ => Delegation::Unknown,
        };

        let repository = repository(&github.repository).map_err(Refused::Failed)?;
        let mut unresolved_blockers = Vec::new();
        for page in 1..=PAGES {
            let response = self
                .request(
                    reqwest::Method::GET,
                    github,
                    &format!("repos/{repository}/issues/{number}/dependencies/blocked_by?per_page={PER_PAGE}&page={page}"),
                )
                .send()
                .await
                .map_err(|error| Refused::Failed(anyhow!("the blockers on {work_item} could not be read: {error}")))?;
            let blockers: Vec<serde_json::Value> =
                answered(response, &format!("the blockers on {work_item}")).await?;
            for blocker in &blockers {
                let url = blocker
                    .get("html_url")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        Refused::Failed(anyhow!("a blocker on {work_item} has no URL"))
                    })?;
                evidence.push(url.to_owned());
                match blocker.get("state").and_then(serde_json::Value::as_str) {
                    Some("open") => unresolved_blockers.push(url.to_owned()),
                    Some("closed") => {}
                    _ => {
                        return Err(Refused::Failed(anyhow!(
                            "a blocker on {work_item} has unknown state"
                        )));
                    }
                }
            }
            if blockers.len() < PER_PAGE {
                return Ok(Readiness {
                    work_item,
                    state,
                    delegation,
                    unresolved_blockers,
                    evidence,
                });
            }
        }
        Err(Refused::Failed(anyhow!(
            "{work_item} has more blockers than one readiness check reads"
        )))
    }

    fn request(
        &self,
        method: reqwest::Method,
        github: &GithubConnection,
        path: &str,
    ) -> reqwest::RequestBuilder {
        self.client
            .request(
                method,
                format!("{}/{path}", github.api.trim_end_matches('/')),
            )
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", VERSION)
            .bearer_auth(github.credential.presented_to_the_external_system())
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

fn said_by_kestrel(comment: &serde_json::Value) -> bool {
    comment
        .get("body")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|body| body.contains(MARKER))
}

fn occurrence(event: &serde_json::Value, github: &GithubConnection) -> Result<Occurrence> {
    let issue = event
        .get("issue")
        .context("a GitHub issue event names no issue")?;
    let event_kind = event
        .get("event")
        .and_then(serde_json::Value::as_str)
        .context("a GitHub issue event has no type")?;

    Ok(Occurrence {
        id: event_id(event)?.to_string(),
        source: source(github),
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
    github: &GithubConnection,
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
        source: source(github),
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
            .or_else(|| self.field(&["sender", "login"]))
            .or_else(|| self.field(&["comment", "user", "login"]))
            .and_then(serde_json::Value::as_str)
    }

    /// GitHub's word for the author's standing in the repository, where an Event carries one.
    pub fn association(&self) -> Option<&str> {
        self.field(&["author_association"])
            .or_else(|| self.field(&["issue", "author_association"]))
            .or_else(|| self.field(&["comment", "author_association"]))
            .and_then(serde_json::Value::as_str)
    }

    pub fn label(&self) -> Option<&str> {
        self.field(&["label", "name"])
            .and_then(serde_json::Value::as_str)
    }

    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.field(&["issue", "labels"])
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|label| label.get("name").and_then(serde_json::Value::as_str))
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

    pub fn message(&self) -> Option<&'a str> {
        self.field(&["body"])
            .or_else(|| self.field(&["comment", "body"]))
            .and_then(serde_json::Value::as_str)
    }

    /// Exactly what a filter's `prefix` on the body sees, so the two never disagree about a command.
    pub fn command(&self) -> Option<Command<'a>> {
        if self.occurrence.r#type != COMMENTED {
            return None;
        }
        let rest = self.message()?.strip_prefix(MENTION)?;
        if rest.starts_with(|character: char| !character.is_whitespace()) {
            return None;
        }

        let rest = rest.trim_start();
        let (agent, rest) = match rest.strip_prefix(AGENT) {
            Some(named) => {
                let end = named.find(char::is_whitespace).unwrap_or(named.len());
                (Some(&named[..end]), named[end..].trim_start())
            }
            None => (None, rest),
        };
        let instruction = rest.trim_end();

        Some(Command {
            agent,
            instruction: (!instruction.is_empty()).then_some(instruction),
        })
    }

    pub fn subject_issue(&self) -> Option<i64> {
        self.occurrence
            .subject
            .as_deref()
            .and_then(|subject| subject.strip_prefix('#'))
            .and_then(|number| number.parse().ok())
    }

    fn field(&self, path: &[&str]) -> Option<&'a serde_json::Value> {
        let mut at = &self.occurrence.data;
        for part in path {
            at = at.get(*part)?;
        }
        Some(at)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command<'a> {
    pub agent: Option<&'a str>,
    pub instruction: Option<&'a str>,
}

/// What an operator's dispatch of `issue` mints: about the issue, as anything GitHub said of it is.
pub fn dispatched(
    github: &GithubConnection,
    r#type: &str,
    issue: i64,
    data: serde_json::Value,
) -> Occurrence {
    Occurrence {
        id: uuid::Uuid::now_v7().to_string(),
        source: source(github),
        specversion: "1.0".to_owned(),
        r#type: r#type.to_owned(),
        subject: Some(format!("#{issue}")),
        time: Timestamp::now(),
        data,
    }
}

/// One webhook delivery, named in GitHub's webhook vocabulary, or nothing for a comment kestrel
/// itself left. A comment keeps the id a poll gives it, so the two ways of learning it dedup.
pub fn delivered(
    github: &GithubConnection,
    event: &str,
    delivery: &str,
    payload: serde_json::Value,
) -> Result<Option<Occurrence>> {
    if let Some(named) = payload
        .get("repository")
        .and_then(|repository| repository.get("full_name"))
        .and_then(serde_json::Value::as_str)
        && !named.eq_ignore_ascii_case(&github.repository)
    {
        bail!(
            "the delivery is about {named}, and this integration watches {}",
            github.repository
        );
    }
    if event == "issue_comment" && payload.get("comment").is_some_and(said_by_kestrel) {
        return Ok(None);
    }

    let action = payload.get("action").and_then(serde_json::Value::as_str);
    let id = match (event, action) {
        ("issue_comment", Some("created")) => format!(
            "comment:{}",
            payload
                .get("comment")
                .map(comment_id)
                .context("an issue_comment delivery names no comment")??
        ),
        _ => delivery.to_owned(),
    };
    let subject = payload
        .get("issue")
        .or_else(|| payload.get("pull_request"))
        .and_then(|issue| issue.get("number"))
        .and_then(serde_json::Value::as_i64)
        .map(|number| format!("#{number}"));

    Ok(Some(Occurrence {
        id,
        source: source(github),
        specversion: "1.0".to_owned(),
        r#type: match action {
            Some(action) => format!("com.github.{event}.{action}"),
            None => format!("com.github.{event}"),
        },
        subject,
        time: Timestamp::now(),
        data: payload,
    }))
}

/// The external resource the event is about (ADR-0011): the repository, never the
/// integration, so an event dedups identically however kestrel learned it.
fn source(github: &GithubConnection) -> String {
    format!("https://github.com/{}", github.repository)
}

/// `owner/name`, checked here because it is pasted into a URL rather than sent as a parameter.
pub fn repository(repository: &str) -> Result<String> {
    let unnamed = || {
        Declined::Unacceptable(format!(
            "{repository} is not a github repository: name it owner/name"
        ))
    };
    let (owner, name) = repository.split_once('/').ok_or_else(unnamed)?;

    for part in [owner, name] {
        if part.is_empty()
            || !part
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
        {
            bail!(unnamed());
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

    fn watching() -> GithubConnection {
        GithubConnection {
            repository: "jtmthf/kestrel".to_owned(),
            api: API.to_owned(),
            credential: crate::integration::credential::Token::held("nothing"),
            interval: jiff::SignedDuration::from_secs(60),
            signed: true,
        }
    }

    #[test]
    fn a_delivery_is_named_in_the_webhook_vocabulary() {
        let payload = serde_json::json!({
            "action": "labeled",
            "label": { "name": "ready-for-agent" },
            "issue": { "number": 43 },
            "repository": { "full_name": "jtmthf/kestrel" },
            "sender": { "login": "jtmthf" }
        });

        let occurrence = delivered(&watching(), "issues", "d-1", payload)
            .unwrap()
            .expect("a label is an event");

        assert_eq!(occurrence.r#type, LABELLED);
        assert_eq!(occurrence.id, "d-1");
        assert_eq!(occurrence.source, "https://github.com/jtmthf/kestrel");
        assert_eq!(occurrence.subject.as_deref(), Some("#43"));
        assert_eq!(EventData::new(&occurrence).actor(), Some("jtmthf"));
    }

    #[test]
    fn a_delivered_comment_is_identified_as_a_polled_one_is() {
        let payload = serde_json::json!({
            "action": "created",
            "comment": { "id": 99, "body": "please add a test" },
            "issue": { "number": 43 }
        });

        let occurrence = delivered(&watching(), "issue_comment", "d-2", payload)
            .unwrap()
            .expect("a comment is an event");

        assert_eq!(occurrence.r#type, COMMENTED);
        assert_eq!(occurrence.id, "comment:99");
    }

    #[test]
    fn a_comment_kestrel_left_is_not_heard_back() {
        let payload = serde_json::json!({
            "action": "created",
            "comment": { "id": 99, "body": "done\n<!-- kestrel run 1 -->" },
            "issue": { "number": 43 }
        });

        assert!(
            delivered(&watching(), "issue_comment", "d-3", payload)
                .unwrap()
                .is_none()
        );
    }

    fn commented(body: &str) -> Occurrence {
        comment_occurrence(
            &serde_json::json!({
                "id": 5,
                "issue_url": "https://api.github.com/repos/jtmthf/kestrel/issues/43",
                "created_at": "2026-09-01T12:00:00Z",
                "user": { "login": "jtmthf" },
                "body": body,
            }),
            &watching(),
        )
        .expect("a comment")
    }

    fn command(body: &str) -> Option<(Option<String>, Option<String>)> {
        let occurrence = commented(body);
        EventData::new(&occurrence).command().map(|command| {
            (
                command.agent.map(str::to_owned),
                command.instruction.map(str::to_owned),
            )
        })
    }

    #[test]
    fn a_comment_opening_with_the_mention_is_a_command() {
        let some = |text: &str| Some(text.to_owned());

        assert_eq!(command("@kestrel"), Some((None, None)));
        assert_eq!(
            command("@kestrel /implement\nthen open a PR\n"),
            Some((None, some("/implement\nthen open a PR")))
        );
        assert_eq!(
            command("@kestrel agent=codex $tdd the parser"),
            Some((some("codex"), some("$tdd the parser")))
        );
        assert_eq!(
            command("@kestrel agent=claude"),
            Some((some("claude"), None))
        );
    }

    #[test]
    fn a_passing_mention_commands_nothing() {
        assert_eq!(command("thanks @kestrel"), None);
        assert_eq!(command("@kestrels are birds"), None);
        assert_eq!(command("@kestrel-bot can you look?"), None);
        assert_eq!(command(" @kestrel go"), None);
        assert_eq!(command("@Kestrel go"), None);
        assert_eq!(command("please add a test"), None);
    }

    #[test]
    fn only_a_comment_can_be_a_command() {
        let mut occurrence = commented("@kestrel /implement");
        occurrence.r#type = LABELLED.to_owned();

        assert_eq!(EventData::new(&occurrence).command(), None);
    }

    #[test]
    fn a_delivered_comment_commands_as_a_polled_one_does() {
        let payload = serde_json::json!({
            "action": "created",
            "comment": { "id": 99, "body": "@kestrel agent=codex go", "user": { "login": "jtmthf" } },
            "issue": { "number": 43 },
            "sender": { "login": "jtmthf" }
        });
        let occurrence = delivered(&watching(), "issue_comment", "d-5", payload)
            .unwrap()
            .expect("a comment is an event");

        assert_eq!(
            EventData::new(&occurrence).command(),
            Some(Command {
                agent: Some("codex"),
                instruction: Some("go"),
            })
        );
    }

    #[test]
    fn a_delivery_about_another_repository_is_refused() {
        let payload = serde_json::json!({
            "action": "labeled",
            "repository": { "full_name": "someone/else" }
        });

        assert!(delivered(&watching(), "issues", "d-4", payload).is_err());
    }

    #[test]
    fn an_issue_event_without_a_producer_id_is_refused() {
        let event = serde_json::json!({
            "event": "labeled",
            "created_at": "2026-09-01T12:00:00Z",
            "issue": { "number": 43 }
        });
        let refusal = occurrence(&event, &watching())
            .expect_err("an event without its producer id should be refused");

        assert!(refusal.to_string().contains("integer id"));
    }
}
