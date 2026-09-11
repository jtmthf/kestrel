pub mod credential;
pub mod github;
pub mod outcome;

use anyhow::{Result, bail};
use jiff::{SignedDuration, Timestamp};
use tracing::warn;

use crate::domain::{Direction, Event, EventId, Integration, IntegrationKind};
use crate::integration::credential::Token;
use crate::integration::github::{Github, Refused};
use crate::store::Store;
use crate::store::integration::Recorded;

pub struct Registration<'a> {
    pub organization: &'a str,
    pub name: &'a str,
    pub kind: IntegrationKind,
    pub repository: &'a str,
    pub api: &'a str,
    pub token: &'a str,
    pub carries: &'a [Direction],
    pub interval: SignedDuration,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Polled {
    pub seen: usize,
    pub recorded: usize,
}

pub async fn register(store: &Store, registration: Registration<'_>) -> Result<Integration> {
    if registration.carries.is_empty() {
        bail!("an integration carries something: name a direction it carries");
    }
    if registration.interval <= SignedDuration::ZERO {
        bail!("a poll interval is how long kestrel waits, and cannot be zero or negative");
    }

    let repository = github::repository(registration.repository)?;

    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(registration.organization).await?;
    let integration = tx
        .integrations()
        .register(
            &organization,
            registration.name,
            registration.kind,
            &repository,
            registration.api,
            &Token::held(registration.token),
            registration.carries,
            registration.interval,
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

pub async fn events(store: &Store, organization: &str, limit: usize) -> Result<Vec<Event>> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.integrations().events(&organization, limit).await
}

pub async fn event(store: &Store, id: EventId) -> Result<Event> {
    store.begin().await?.integrations().event(id).await
}

/// One poll of one Integration. Every Event the poll saw and what it was polled through are
/// written in one transaction, so a poll that is interrupted before it commits leaves the
/// Integration where it was and the next one covers the same window again — which costs
/// nothing, because an Event already recorded is recognised rather than recorded twice.
pub async fn poll(store: &Store, github: &Github, integration: &Integration) -> Result<Polled> {
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
                |refused| back_off(integration, refused),
                |_| Timestamp::now() + integration.interval,
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
fn back_off(integration: &Integration, refused: &Refused) -> Timestamp {
    let interval = Timestamp::now() + integration.interval;

    match refused {
        Refused::RateLimited { until } => interval.max(*until),
        Refused::Failed(_) => interval,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;
    use crate::domain::{IntegrationId, OrganizationId};

    fn an_integration(interval: SignedDuration) -> Integration {
        Integration {
            id: IntegrationId::generate(),
            organization: OrganizationId::generate(),
            name: "github".to_owned(),
            kind: IntegrationKind::Github,
            repository: "jtmthf/kestrel".to_owned(),
            api: github::API.to_owned(),
            credential: Token::held("ghp_nothing"),
            carries: vec![Direction::Inbound],
            interval,
            poll_due_at: Some(Timestamp::now()),
            polled_through: None,
            comments_polled_through: None,
        }
    }

    #[test]
    fn a_rate_limit_defers_the_next_poll_to_the_moment_it_lifts() {
        let integration = an_integration(SignedDuration::from_secs(1));
        let until = Timestamp::now() + SignedDuration::from_hours(1);

        assert_eq!(
            back_off(&integration, &Refused::RateLimited { until }),
            until
        );
    }

    #[test]
    fn a_rate_limit_that_has_already_lifted_still_waits_the_interval_out() {
        let integration = an_integration(SignedDuration::from_secs(60));
        let until = Timestamp::now() - SignedDuration::from_hours(1);

        assert!(back_off(&integration, &Refused::RateLimited { until }) > Timestamp::now());
    }

    #[test]
    fn a_failure_waits_the_interval_out_rather_than_hammering() {
        let integration = an_integration(SignedDuration::from_secs(60));

        assert!(
            back_off(
                &integration,
                &Refused::Failed(anyhow!("connection refused"))
            ) > Timestamp::now() + SignedDuration::from_secs(50)
        );
    }
}
