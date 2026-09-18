pub mod credential;
pub mod github;
pub mod outcome;
pub mod webhook;

use anyhow::{Result, bail};
use jiff::{SignedDuration, Timestamp};
use tracing::warn;

use crate::declined::Declined;
use crate::domain::{Connection, Direction, Event, EventRecordId, GithubConnection, Integration};
use crate::integration::credential::Token;
use crate::integration::github::{Github, Refused};
use crate::store::Store;
use crate::store::integration::Recorded;

pub struct Registration<'a> {
    pub organization: &'a str,
    pub name: &'a str,
    pub carries: &'a [Direction],
    pub connecting: Connecting<'a>,
}

pub enum Connecting<'a> {
    /// A `signing_secret` means GitHub delivers by webhook, and the repository is not polled.
    Github {
        repository: &'a str,
        api: &'a str,
        token: &'a str,
        interval: SignedDuration,
        signing_secret: Option<&'a str>,
    },
    Webhook {
        secret: &'a str,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Polled {
    pub seen: usize,
    pub recorded: usize,
}

pub async fn register(store: &Store, registration: Registration<'_>) -> Result<Integration> {
    if registration.carries.is_empty() {
        bail!(Declined::Unacceptable(
            "an integration carries something: name a direction it carries".to_owned()
        ));
    }

    let (connection, webhook_secret) = match registration.connecting {
        Connecting::Github {
            repository,
            api,
            token,
            interval,
            signing_secret,
        } => {
            if interval <= SignedDuration::ZERO {
                bail!(Declined::Unacceptable(
                    "a poll interval is how long kestrel waits, and cannot be zero or negative"
                        .to_owned()
                ));
            }
            (
                Connection::Github(GithubConnection {
                    repository: github::repository(repository)?,
                    api: api.to_owned(),
                    credential: Token::held(token),
                    interval,
                    signed: signing_secret.is_some(),
                }),
                signing_secret,
            )
        }
        Connecting::Webhook { secret } => {
            if registration.carries.contains(&Direction::Outbound) {
                bail!(Declined::Unacceptable(
                    "a generic webhook carries events inbound only".to_owned()
                ));
            }
            (Connection::Webhook, Some(secret))
        }
    };
    if webhook_secret.is_some_and(str::is_empty) {
        bail!(Declined::Unacceptable(
            "a webhook secret with nothing in it is not one".to_owned()
        ));
    }

    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(registration.organization).await?;
    let integration = tx
        .integrations()
        .register(
            &organization,
            registration.name,
            connection,
            registration.carries,
            webhook_secret,
        )
        .await?;
    tx.commit().await?;

    Ok(integration)
}

pub async fn integrations(store: &Store, organization: &str) -> Result<Vec<Integration>> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.integrations().all(&organization).await
}

pub async fn acknowledge_event_refusal(
    store: &Store,
    organization: &str,
    name: &str,
) -> Result<()> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;
    let integration = tx.integrations().named(&organization, name).await?;
    tx.integrations()
        .acknowledge_event_refusal(&integration)
        .await?;
    tx.commit().await
}

pub async fn events(store: &Store, organization: &str, limit: usize) -> Result<Vec<Event>> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.integrations().events(&organization, limit).await
}

pub async fn event(store: &Store, id: EventRecordId) -> Result<Event> {
    store.begin().await?.integrations().event(id).await
}

/// One poll of one Integration. Every Event the poll saw and what it was polled through are
/// written in one transaction, so a poll that is interrupted before it commits leaves the
/// Integration where it was and the next one covers the same window again — which costs
/// nothing, because an Event already recorded is recognised rather than recorded twice.
pub async fn poll(store: &Store, github: &Github, integration: &Integration) -> Result<Polled> {
    let interval = integration.github()?.interval;
    let seen = github.issue_events(integration).await;
    let comments = github.issue_comments(integration).await;
    let mut tx = store.begin().await?;
    let mut recorded = 0;

    if let Ok(seen) = &seen {
        for occurrence in &seen.occurrences {
            match tx
                .integrations()
                .record_event(integration, occurrence)
                .await?
            {
                Recorded::Recorded => recorded += 1,
                Recorded::Already => {}
                Recorded::Refused { because } => {
                    warn!(
                        integration = integration.name,
                        %because,
                        "an event was refused at ingest rather than stored"
                    );
                }
            }
        }
    } else if let Err(refused) = &seen {
        warn!(
            integration = integration.name,
            because = %refused,
            "an event poll came back with nothing"
        );
    }
    if let Ok(comments) = &comments {
        for occurrence in &comments.occurrences {
            match tx
                .integrations()
                .record_event(integration, occurrence)
                .await?
            {
                Recorded::Recorded => recorded += 1,
                Recorded::Already => {}
                Recorded::Refused { because } => {
                    warn!(
                        integration = integration.name,
                        %because,
                        "an event was refused at ingest rather than stored"
                    );
                }
            }
        }
        tx.integrations()
            .comments_polled(integration, comments.through)
            .await?;
    } else if let Err(refused) = &comments {
        warn!(
            integration = integration.name,
            because = %refused,
            "a comment poll came back with nothing"
        );
    }
    tx.integrations()
        .polled(
            integration,
            seen.as_ref()
                .map_or(integration.polled_through, |seen| seen.through),
            seen.as_ref().map_or_else(
                |refused| back_off(interval, refused),
                |_| Timestamp::now() + interval,
            ),
        )
        .await?;
    tx.commit().await?;

    Ok(Polled {
        seen: seen.as_ref().map_or(0, |seen| seen.occurrences.len())
            + comments
                .as_ref()
                .map_or(0, |comments| comments.occurrences.len()),
        recorded,
    })
}

/// A rate limit names the moment it lifts, and asking again before then spends a request on
/// another refusal; nothing is gained by polling more often than the interval either way.
fn back_off(interval: SignedDuration, refused: &Refused) -> Timestamp {
    let next = Timestamp::now() + interval;

    match refused {
        Refused::RateLimited { until } => next.max(*until),
        Refused::Failed(_) => next,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn a_rate_limit_defers_the_next_poll_to_the_moment_it_lifts() {
        let until = Timestamp::now() + SignedDuration::from_hours(1);

        assert_eq!(
            back_off(
                SignedDuration::from_secs(1),
                &Refused::RateLimited { until }
            ),
            until
        );
    }

    #[test]
    fn a_rate_limit_that_has_already_lifted_still_waits_the_interval_out() {
        let until = Timestamp::now() - SignedDuration::from_hours(1);

        assert!(
            back_off(
                SignedDuration::from_secs(60),
                &Refused::RateLimited { until }
            ) > Timestamp::now()
        );
    }

    #[test]
    fn a_failure_waits_the_interval_out_rather_than_hammering() {
        assert!(
            back_off(
                SignedDuration::from_secs(60),
                &Refused::Failed(anyhow!("connection refused"))
            ) > Timestamp::now() + SignedDuration::from_secs(50)
        );
    }
}
