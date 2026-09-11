use anyhow::{Context as _, Result};
use jiff::{SignedDuration, Timestamp};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use crate::domain::{
    Direction, Event, EventId, Integration, IntegrationId, IntegrationKind, Occurrence,
    Organization, Outcome, Run, Session,
};
use crate::integration::credential::Token;
use crate::store::{due, session, timestamp};

/// The largest payload an Event may carry. An event stream is systems kestrel does not
/// control, so one that overflows the store is refused rather than grown to fit.
const MAX_EVENT_BYTES: usize = 1024 * 1024;

pub enum Recorded {
    Recorded,
    Already,
    Refused { because: String },
}

macro_rules! integrations_where {
    ($tail:literal) => {
        concat!(
            "SELECT id, organization_id, name, kind, repository, api, credential, inbound,
                    outbound, interval_ms, poll_due_at, polled_through, comments_polled_through
             FROM integration
             WHERE ",
            $tail
        )
    };
}

pub struct Integrations<'a> {
    connection: &'a mut SqliteConnection,
}

impl<'a> Integrations<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection) -> Self {
        Self { connection }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "an integration is what it is declared with"
    )]
    pub async fn register(
        &mut self,
        organization: &Organization,
        name: &str,
        kind: IntegrationKind,
        repository: &str,
        api: &str,
        credential: &Token,
        carries: &[Direction],
        interval: SignedDuration,
    ) -> Result<Integration> {
        let inbound = carries.contains(&Direction::Inbound);
        let integration = Integration {
            id: IntegrationId::generate(),
            organization: organization.id,
            name: name.to_owned(),
            kind,
            repository: repository.to_owned(),
            api: api.to_owned(),
            credential: credential.clone(),
            carries: carries.to_vec(),
            interval,
            // Due the moment it is registered, so an operator who registers one sees what is
            // on the repository rather than waiting an interval to find out.
            poll_due_at: inbound.then(Timestamp::now),
            polled_through: None,
            comments_polled_through: None,
        };

        sqlx::query(
            "INSERT INTO integration
                 (id, organization_id, name, kind, repository, api, credential, inbound,
                  outbound, interval_ms, poll_due_at, registered_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(integration.id.to_string())
        .bind(integration.organization.to_string())
        .bind(&integration.name)
        .bind(integration.kind.as_str())
        .bind(&integration.repository)
        .bind(&integration.api)
        .bind(credential.presented_to_the_external_system())
        .bind(inbound)
        .bind(carries.contains(&Direction::Outbound))
        .bind(i64::try_from(interval.as_millis())?)
        .bind(integration.poll_due_at.map(due))
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("registering the integration {name}"))?;

        Ok(integration)
    }

    pub async fn all(&mut self, organization: &Organization) -> Result<Vec<Integration>> {
        sqlx::query(integrations_where!("organization_id = ? ORDER BY name"))
            .bind(organization.id.to_string())
            .fetch_all(&mut *self.connection)
            .await?
            .iter()
            .map(integration)
            .collect()
    }

    /// Only an Integration that carries events inbound is polled: the direction it declares
    /// is what it does, rather than a label beside it.
    pub async fn due(&mut self, at: Timestamp) -> Result<Vec<Integration>> {
        sqlx::query(integrations_where!(
            "inbound = TRUE AND poll_due_at <= ? ORDER BY poll_due_at"
        ))
        .bind(due(at))
        .fetch_all(&mut *self.connection)
        .await
        .context("reading which integrations are due a poll")?
        .iter()
        .map(integration)
        .collect()
    }

    pub async fn with_id(&mut self, id: IntegrationId) -> Result<Integration> {
        let row = sqlx::query(integrations_where!("id = ?"))
            .bind(id.to_string())
            .fetch_optional(&mut *self.connection)
            .await?
            .with_context(|| format!("no integration {id}"))?;

        integration(&row)
    }

    /// `Already` when the Event was recorded by an earlier poll whose window overlapped this
    /// one: an Event is identified by the `(source, id)` CloudEvents call it, and recorded
    /// once however kestrel learned it. `Refused` when the payload is too large to hold:
    /// a refused Event is skipped and never stored, rather than truncated.
    pub async fn record_event(
        &mut self,
        integration: &Integration,
        occurrence: &Occurrence,
    ) -> Result<Recorded> {
        let data = serde_json::to_string(&occurrence.data)
            .with_context(|| format!("the event {} does not read as JSON", occurrence.id))?;
        if data.len() > MAX_EVENT_BYTES {
            return Ok(Recorded::Refused {
                because: format!(
                    "the payload of the event {} is {} bytes, over the {} allowed",
                    occurrence.id,
                    data.len(),
                    MAX_EVENT_BYTES
                ),
            });
        }

        let recorded = sqlx::query(
            "INSERT INTO event
                 (id, organization_id, integration_id, source, external_id, type, subject,
                  time, data, recorded_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (source, external_id) DO NOTHING",
        )
        .bind(EventId::generate().to_string())
        .bind(integration.organization.to_string())
        .bind(integration.id.to_string())
        .bind(&occurrence.source)
        .bind(&occurrence.id)
        .bind(&occurrence.r#type)
        .bind(occurrence.subject.as_deref())
        .bind(occurrence.time.to_string())
        .bind(&data)
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| {
            format!(
                "recording the event {} on {}",
                occurrence.id, occurrence.source
            )
        })?;

        Ok(if recorded.rows_affected() > 0 {
            Recorded::Recorded
        } else {
            Recorded::Already
        })
    }

    pub async fn polled(
        &mut self,
        integration: &Integration,
        through: Option<i64>,
        due_again_at: Timestamp,
    ) -> Result<()> {
        sqlx::query("UPDATE integration SET polled_through = ?, poll_due_at = ? WHERE id = ?")
            .bind(through)
            .bind(due(due_again_at))
            .bind(integration.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording the poll of integration {}", integration.name))?;

        Ok(())
    }

    pub async fn comments_polled(
        &mut self,
        integration: &Integration,
        through: Option<i64>,
    ) -> Result<()> {
        sqlx::query("UPDATE integration SET comments_polled_through = ? WHERE id = ?")
            .bind(through)
            .bind(integration.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| {
                format!(
                    "recording which comments integration {} has seen",
                    integration.name
                )
            })?;

        Ok(())
    }

    pub async fn events(
        &mut self,
        organization: &Organization,
        limit: usize,
    ) -> Result<Vec<Event>> {
        sqlx::query(
            "SELECT id, organization_id, integration_id, source, external_id, type, subject,
                    time, data, recorded_at
             FROM event
             WHERE organization_id = ?
             ORDER BY time DESC, external_id DESC
             LIMIT ?",
        )
        .bind(organization.id.to_string())
        .bind(i64::try_from(limit)?)
        .fetch_all(&mut *self.connection)
        .await?
        .iter()
        .map(event)
        .collect()
    }

    pub async fn event(&mut self, id: EventId) -> Result<Event> {
        event_with_id(self.connection, id).await
    }

    /// An Event nothing still needs is forgotten rather than orphaned: whatever a Session, a
    /// firing, a follow-up or an outcome looks back at stays, and reaping is a deletion only.
    pub async fn reap_events(&mut self, expired_before: Timestamp) -> Result<usize> {
        let expired = sqlx::query(
            "DELETE FROM event
             WHERE recorded_at < ?
               AND NOT EXISTS (SELECT 1 FROM firing WHERE firing.event_id = event.id)
               AND NOT EXISTS (SELECT 1 FROM follow_up WHERE follow_up.event_id = event.id)
               AND NOT EXISTS (SELECT 1 FROM outcome WHERE outcome.event_id = event.id)
               AND NOT EXISTS (SELECT 1 FROM session WHERE session.event_id = event.id)",
        )
        .bind(expired_before.to_string())
        .execute(&mut *self.connection)
        .await
        .context("forgetting events older than the retention window")?;

        Ok(expired.rows_affected() as usize)
    }

    pub async fn unfollowed(&mut self, r#type: &str, limit: usize) -> Result<Vec<Event>> {
        sqlx::query(
            "SELECT event.id, event.organization_id, event.integration_id, event.source,
                    event.external_id, event.type, event.subject, event.time, event.data,
                    event.recorded_at
             FROM event
             LEFT JOIN follow_up ON follow_up.event_id = event.id
             WHERE event.type = ? AND follow_up.event_id IS NULL
               AND EXISTS (
                   SELECT 1
                   FROM session
                   JOIN event AS origin ON origin.id = session.event_id
                   WHERE session.organization_id = event.organization_id
                     AND origin.integration_id = event.integration_id
                     AND origin.source = event.source
                     AND origin.subject = event.subject
                     AND origin.time <= event.time
               )
             ORDER BY event.time, event.external_id
             LIMIT ?",
        )
        .bind(r#type)
        .bind(i64::try_from(limit)?)
        .fetch_all(&mut *self.connection)
        .await?
        .iter()
        .map(event)
        .collect()
    }

    pub async fn session_for_follow_up(&mut self, event: &Event) -> Result<Option<Session>> {
        let found = sqlx::query(
            "SELECT session.id
             FROM session
             JOIN event AS origin ON origin.id = session.event_id
             WHERE session.organization_id = ?
               AND origin.integration_id = ?
               AND origin.source = ?
               AND origin.subject = ?
               AND origin.time <= ?
             ORDER BY session.opened_at DESC, session.id DESC
             LIMIT 1",
        )
        .bind(event.organization.to_string())
        .bind(event.integration.to_string())
        .bind(&event.occurrence.source)
        .bind(event.occurrence.subject.as_deref())
        .bind(event.occurrence.time.to_string())
        .fetch_optional(&mut *self.connection)
        .await?;

        match found {
            Some(row) => Ok(Some(
                session::read(self.connection, row.get::<String, _>("id").parse()?).await?,
            )),
            None => Ok(None),
        }
    }

    pub async fn record_follow_up(&mut self, event: &Event, session: &Session) -> Result<()> {
        sqlx::query(
            "INSERT INTO follow_up (event_id, organization_id, session_id, received_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(event.id.to_string())
        .bind(event.organization.to_string())
        .bind(session.id.to_string())
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("recording the follow-up event {}", event.id))?;

        Ok(())
    }

    /// Due the moment it is recorded, and recorded in the transaction that ends the Run, so a
    /// Run that ended has an outcome to deliver and one that did not has nothing to withdraw.
    pub async fn record_outcome(
        &mut self,
        run: &Run,
        integration: &Integration,
        event: &Event,
        body: &str,
    ) -> Result<()> {
        let subject = event.occurrence.subject_issue().with_context(|| {
            format!(
                "the event {} names no issue the outcome could reach",
                event.id
            )
        })?;

        sqlx::query(
            "INSERT INTO outcome
                 (run_id, organization_id, integration_id, event_id, subject, body, due_at,
                  recorded_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (run_id) DO NOTHING",
        )
        .bind(run.id.to_string())
        .bind(run.organization.to_string())
        .bind(integration.id.to_string())
        .bind(event.id.to_string())
        .bind(subject)
        .bind(body)
        .bind(due(Timestamp::now()))
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("recording what to say back about the run {}", run.id))?;

        Ok(())
    }

    pub async fn outcomes_due(&mut self, at: Timestamp) -> Result<Vec<Outcome>> {
        sqlx::query(
            "SELECT run_id, organization_id, integration_id, event_id, subject, body,
                    attempted_at
             FROM outcome
             WHERE due_at <= ?
             ORDER BY due_at",
        )
        .bind(due(at))
        .fetch_all(&mut *self.connection)
        .await
        .context("reading which outcomes are due a delivery")?
        .iter()
        .map(outcome)
        .collect()
    }

    /// Committed before the request goes out rather than after it comes back: what this
    /// records is that a comment may now exist, which is true from the moment kestrel asks.
    pub async fn attempting_outcome(&mut self, outcome: &Outcome, at: Timestamp) -> Result<()> {
        sqlx::query("UPDATE outcome SET attempted_at = ? WHERE run_id = ?")
            .bind(at.to_string())
            .bind(outcome.run.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| {
                format!("recording an attempt at the outcome of run {}", outcome.run)
            })?;

        Ok(())
    }

    pub async fn outcome_delivered(&mut self, outcome: &Outcome, to: &str) -> Result<()> {
        sqlx::query(
            "UPDATE outcome SET delivered_at = ?, delivered_to = ?, due_at = NULL
             WHERE run_id = ?",
        )
        .bind(Timestamp::now().to_string())
        .bind(to)
        .bind(outcome.run.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("recording the outcome of run {} as said", outcome.run))?;

        Ok(())
    }

    pub async fn outcome_deferred(
        &mut self,
        outcome: &Outcome,
        due_again_at: Timestamp,
    ) -> Result<()> {
        sqlx::query("UPDATE outcome SET due_at = ? WHERE run_id = ?")
            .bind(due(due_again_at))
            .bind(outcome.run.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("deferring the outcome of run {}", outcome.run))?;

        Ok(())
    }
}

pub(crate) async fn event_with_id(connection: &mut SqliteConnection, id: EventId) -> Result<Event> {
    let row = sqlx::query(
        "SELECT id, organization_id, integration_id, source, external_id, type, subject,
                time, data, recorded_at
         FROM event
         WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(&mut *connection)
    .await?
    .with_context(|| format!("no event {id}"))?;

    event(&row)
}

fn integration(row: &SqliteRow) -> Result<Integration> {
    let mut carries = Vec::new();
    if row.get::<bool, _>("inbound") {
        carries.push(Direction::Inbound);
    }
    if row.get::<bool, _>("outbound") {
        carries.push(Direction::Outbound);
    }

    Ok(Integration {
        id: row.get::<String, _>("id").parse()?,
        organization: row.get::<String, _>("organization_id").parse()?,
        name: row.get("name"),
        kind: row.get::<String, _>("kind").parse()?,
        repository: row.get("repository"),
        api: row.get("api"),
        credential: Token::held(row.get("credential")),
        carries,
        interval: SignedDuration::from_millis(row.get("interval_ms")),
        poll_due_at: timestamp(row, "poll_due_at")?,
        polled_through: row.get("polled_through"),
        comments_polled_through: row.get("comments_polled_through"),
    })
}

fn event(row: &SqliteRow) -> Result<Event> {
    Ok(Event {
        id: row.get::<String, _>("id").parse()?,
        organization: row.get::<String, _>("organization_id").parse()?,
        integration: row.get::<String, _>("integration_id").parse()?,
        occurrence: Occurrence {
            id: row.get("external_id"),
            source: row.get("source"),
            r#type: row.get("type"),
            subject: row.get("subject"),
            time: row.get::<String, _>("time").parse()?,
            data: serde_json::from_str(row.get("data"))?,
        },
        recorded_at: row.get::<String, _>("recorded_at").parse()?,
    })
}

fn outcome(row: &SqliteRow) -> Result<Outcome> {
    Ok(Outcome {
        run: row.get::<String, _>("run_id").parse()?,
        organization: row.get::<String, _>("organization_id").parse()?,
        integration: row.get::<String, _>("integration_id").parse()?,
        event: row.get::<String, _>("event_id").parse()?,
        subject: row.get("subject"),
        body: row.get("body"),
        attempted_at: timestamp(row, "attempted_at")?,
    })
}
