use anyhow::{Context as _, Result};
use jiff::{SignedDuration, Timestamp};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use crate::declined::Declined;
use crate::domain::{
    Connection, Delivery, Direction, Event, EventRecordId, EventRefusal, GithubConnection,
    Integration, IntegrationId, IntegrationKind, Occurrence, Organization, OrganizationId, Run,
    RunId, Session,
};
use crate::integration::credential::Token;
use crate::integration::webhook::Verifier;
use crate::keyring::Keyring;
use crate::link::credential::Secret;
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
                    outbound, interval_ms, signing_secret IS NOT NULL AS signed, poll_due_at,
                    polled_through, comments_polled_through,
                    last_event_refusal_source, last_event_refusal_id, last_event_refusal_bytes,
                    last_event_refusal_reason, last_event_refusal_at
             FROM integration
             WHERE ",
            $tail
        )
    };
}

pub struct Integrations<'a> {
    connection: &'a mut SqliteConnection,
    keyring: &'a Keyring,
}

impl<'a> Integrations<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection, keyring: &'a Keyring) -> Self {
        Self {
            connection,
            keyring,
        }
    }

    /// `webhook_secret` is sealed for a GitHub Integration, which signs with it, and digested
    /// for a generic one, whose sender presents it.
    pub async fn register(
        &mut self,
        organization: &Organization,
        name: &str,
        connection: Connection,
        carries: &[Direction],
        webhook_secret: Option<&str>,
    ) -> Result<Integration> {
        let polled = matches!(&connection, Connection::Github(github) if !github.signed)
            && carries.contains(&Direction::Inbound);
        let integration = Integration {
            id: IntegrationId::generate(),
            organization: organization.id,
            name: name.to_owned(),
            connection,
            carries: carries.to_vec(),
            // Due the moment it is registered, so an operator who registers one sees what is
            // on the repository rather than waiting an interval to find out.
            poll_due_at: polled.then(Timestamp::now),
            polled_through: None,
            comments_polled_through: None,
            last_event_refusal: None,
        };
        let github = integration.github().ok();
        let (signing_secret, shared_secret_digest) = match (&integration.connection, webhook_secret)
        {
            (_, None) => (None, None),
            (Connection::Github(_), Some(secret)) => (
                Some(self.keyring.seal(&bound_to(integration.id), secret)?),
                None,
            ),
            (Connection::Webhook, Some(secret)) => (None, Some(Secret::presented(secret).digest())),
        };

        sqlx::query(
            "INSERT INTO integration
                 (id, organization_id, name, kind, repository, api, credential, inbound,
                  outbound, interval_ms, signing_secret, shared_secret_digest, poll_due_at,
                  registered_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(integration.id.to_string())
        .bind(integration.organization.to_string())
        .bind(&integration.name)
        .bind(integration.kind().as_str())
        .bind(github.map(|github| &github.repository))
        .bind(github.map(|github| &github.api))
        .bind(github.map(|github| github.credential.presented_to_the_external_system()))
        .bind(carries.contains(&Direction::Inbound))
        .bind(carries.contains(&Direction::Outbound))
        .bind(
            github
                .map(|github| i64::try_from(github.interval.as_millis()))
                .transpose()?,
        )
        .bind(signing_secret)
        .bind(shared_secret_digest)
        .bind(integration.poll_due_at.map(due))
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .map_err(|error| match error.as_database_error() {
            Some(refused) if refused.is_unique_violation() => Declined::Taken(format!(
                "the organization {} already has an integration named {name}",
                organization.name
            ))
            .into(),
            _ => anyhow::Error::new(error).context(format!("registering the integration {name}")),
        })?;

        Ok(integration)
    }

    /// The one place a GitHub signing secret is decrypted.
    pub async fn verifier(&mut self, integration: &Integration) -> Result<Option<Verifier>> {
        let row = sqlx::query(
            "SELECT signing_secret, shared_secret_digest FROM integration WHERE id = ?",
        )
        .bind(integration.id.to_string())
        .fetch_one(&mut *self.connection)
        .await
        .with_context(|| format!("reading how integration {} is verified", integration.name))?;

        if let Some(sealed) = row.get::<Option<String>, _>("signing_secret") {
            return Ok(Some(Verifier::Signing(
                self.keyring
                    .unseal(&bound_to(integration.id), &sealed)
                    .with_context(|| {
                        format!(
                            "opening the signing secret of integration {}",
                            integration.name
                        )
                    })?,
            )));
        }

        Ok(row
            .get::<Option<String>, _>("shared_secret_digest")
            .map(|digest| Verifier::Shared { digest }))
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

    pub async fn find(&mut self, id: IntegrationId) -> Result<Option<Integration>> {
        sqlx::query(integrations_where!("id = ?"))
            .bind(id.to_string())
            .fetch_optional(&mut *self.connection)
            .await?
            .as_ref()
            .map(integration)
            .transpose()
    }

    pub async fn named(&mut self, organization: &Organization, name: &str) -> Result<Integration> {
        let row = sqlx::query(integrations_where!("organization_id = ? AND name = ?"))
            .bind(organization.id.to_string())
            .bind(name)
            .fetch_optional(&mut *self.connection)
            .await?
            .ok_or_else(|| {
                Declined::Missing(format!(
                    "no integration named {name} in the organization {}",
                    organization.name
                ))
            })?;

        integration(&row)
    }

    pub async fn record_event(
        &mut self,
        integration: &Integration,
        occurrence: &Occurrence,
    ) -> Result<Recorded> {
        if occurrence.specversion != "1.0" {
            anyhow::bail!(
                "the event {} uses unsupported CloudEvents specversion {}",
                occurrence.id,
                occurrence.specversion
            );
        }
        let data = serde_json::to_string(&occurrence.data)
            .with_context(|| format!("the event {} does not read as JSON", occurrence.id))?;
        if data.len() > MAX_EVENT_BYTES {
            let reason = format!(
                "the payload is {} bytes, over the {} allowed",
                data.len(),
                MAX_EVENT_BYTES
            );
            sqlx::query(
                "UPDATE integration
                 SET last_event_refusal_source = ?, last_event_refusal_id = ?,
                     last_event_refusal_bytes = ?, last_event_refusal_reason = ?,
                     last_event_refusal_at = ?
                 WHERE id = ?",
            )
            .bind(&occurrence.source)
            .bind(&occurrence.id)
            .bind(i64::try_from(data.len())?)
            .bind(&reason)
            .bind(Timestamp::now().to_string())
            .bind(integration.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording why the event {} was refused", occurrence.id))?;

            return Ok(Recorded::Refused { because: reason });
        }

        insert_event(
            self.connection,
            integration.organization,
            Some(integration.id),
            occurrence,
            &data,
        )
        .await
    }

    /// Carries no Integration and so none of an Integration's refusals: kestrel is the producer.
    pub async fn record_minted(
        &mut self,
        organization: &Organization,
        occurrence: &Occurrence,
    ) -> Result<Recorded> {
        let data = serde_json::to_string(&occurrence.data)?;
        insert_event(self.connection, organization.id, None, occurrence, &data).await
    }

    pub async fn acknowledge_event_refusal(&mut self, integration: &Integration) -> Result<()> {
        sqlx::query(
            "UPDATE integration
             SET last_event_refusal_source = NULL, last_event_refusal_id = NULL,
                 last_event_refusal_bytes = NULL, last_event_refusal_reason = NULL,
                 last_event_refusal_at = NULL
             WHERE id = ?",
        )
        .bind(integration.id.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| {
            format!(
                "acknowledging the event refusal on integration {}",
                integration.name
            )
        })?;

        Ok(())
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
            "SELECT record_id, organization_id, integration_id, id, source, specversion, type,
                    subject, time, data, recorded_at
             FROM event
             WHERE organization_id = ?
             ORDER BY time DESC, id DESC
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

    pub async fn event(&mut self, id: EventRecordId) -> Result<Event> {
        event_with_id(self.connection, id).await
    }

    pub async fn recorded_event(
        &mut self,
        organization: &Organization,
        occurrence: &Occurrence,
    ) -> Result<Event> {
        let row = sqlx::query(
            "SELECT record_id, organization_id, integration_id, id, source, specversion, type,
                    subject, time, data, recorded_at
             FROM event
             WHERE organization_id = ? AND source = ? AND id = ?",
        )
        .bind(organization.id.to_string())
        .bind(&occurrence.source)
        .bind(&occurrence.id)
        .fetch_optional(&mut *self.connection)
        .await?
        .with_context(|| format!("no event {} on {}", occurrence.id, occurrence.source))?;

        event(&row)
    }

    pub async fn unfollowed(&mut self, r#type: &str, limit: usize) -> Result<Vec<Event>> {
        sqlx::query(
            "SELECT event.record_id, event.organization_id, event.integration_id, event.id,
                    event.source, event.specversion, event.type, event.subject, event.time,
                    event.data, event.recorded_at
             FROM event
             LEFT JOIN follow_up ON follow_up.event_record_id = event.record_id
             WHERE event.type = ? AND follow_up.event_record_id IS NULL
               AND EXISTS (
                   SELECT 1
                   FROM session
                   JOIN event AS origin ON origin.record_id = session.event_record_id
                   WHERE session.organization_id = event.organization_id
                     AND origin.integration_id = event.integration_id
                     AND origin.source = event.source
                     AND origin.subject = event.subject
                     AND origin.time <= event.time
               )
             ORDER BY event.time, event.id
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
             JOIN event AS origin ON origin.record_id = session.event_record_id
             WHERE session.organization_id = ?
               AND origin.integration_id = ?
               AND origin.source = ?
               AND origin.subject = ?
               AND origin.time <= ?
             ORDER BY session.opened_at DESC, session.id DESC
             LIMIT 1",
        )
        .bind(event.organization.to_string())
        .bind(event.integration.map(|integration| integration.to_string()))
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
            "INSERT INTO follow_up (event_record_id, organization_id, session_id, received_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(event.record_id.to_string())
        .bind(event.organization.to_string())
        .bind(session.id.to_string())
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("recording the follow-up event {}", event.record_id))?;

        Ok(())
    }

    /// Due the moment it is recorded, and recorded in the transaction that answered the Turn or
    /// ended the Run, so what kestrel said has a delivery waiting and what it did not say has
    /// nothing to withdraw.
    pub async fn record_delivery(
        &mut self,
        run: &Run,
        integration: &Integration,
        event: &Event,
        turn: Option<i64>,
        body: &str,
        turn_messages: Option<&[String]>,
    ) -> Result<()> {
        let subject = crate::integration::github::EventData::new(&event.occurrence)
            .subject_issue()
            .with_context(|| {
                format!(
                    "the event {} names no issue a delivery could reach",
                    event.record_id
                )
            })?;

        sqlx::query(
            "INSERT INTO delivery
                 (run_id, turn, organization_id, integration_id, event_record_id, subject, body, turn_messages,
                  due_at, recorded_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (run_id, turn) DO NOTHING",
        )
        .bind(run.id.to_string())
        .bind(turn.unwrap_or(0))
        .bind(run.organization.to_string())
        .bind(integration.id.to_string())
        .bind(event.record_id.to_string())
        .bind(subject)
        .bind(body)
        .bind(turn_messages.map(serde_json::to_string).transpose()?)
        .bind(due(Timestamp::now()))
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("recording what to say back about the run {}", run.id))?;

        Ok(())
    }

    pub async fn turn_responses(&mut self, run: RunId) -> Result<Vec<Vec<String>>> {
        let rows = sqlx::query("SELECT turn_messages FROM delivery WHERE run_id = ? AND turn > 0")
            .bind(run.to_string())
            .fetch_all(&mut *self.connection)
            .await
            .with_context(|| format!("reading what run {run} has said back"))?;

        rows.iter()
            .map(|row| Ok(serde_json::from_str(row.get("turn_messages"))?))
            .collect()
    }

    pub async fn deliveries_due(&mut self, at: Timestamp) -> Result<Vec<Delivery>> {
        sqlx::query(
            "SELECT run_id, turn, organization_id, integration_id, event_record_id, subject, body,
                    attempted_at
             FROM delivery
             WHERE due_at <= ?
             ORDER BY due_at",
        )
        .bind(due(at))
        .fetch_all(&mut *self.connection)
        .await
        .context("reading which deliveries are due")?
        .iter()
        .map(delivery)
        .collect()
    }

    /// Committed before the request goes out rather than after it comes back: what this
    /// records is that a comment may now exist, which is true from the moment kestrel asks.
    pub async fn attempting_delivery(&mut self, delivery: &Delivery, at: Timestamp) -> Result<()> {
        sqlx::query("UPDATE delivery SET attempted_at = ? WHERE run_id = ? AND turn = ?")
            .bind(at.to_string())
            .bind(delivery.run.to_string())
            .bind(delivery.turn.unwrap_or(0))
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording an attempt at what run {} said", delivery.run))?;

        Ok(())
    }

    pub async fn delivery_delivered(&mut self, delivery: &Delivery, to: &str) -> Result<()> {
        sqlx::query(
            "UPDATE delivery SET delivered_at = ?, delivered_to = ?, due_at = NULL
             WHERE run_id = ? AND turn = ?",
        )
        .bind(Timestamp::now().to_string())
        .bind(to)
        .bind(delivery.run.to_string())
        .bind(delivery.turn.unwrap_or(0))
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("recording what run {} said as delivered", delivery.run))?;

        Ok(())
    }

    pub async fn delivery_deferred(
        &mut self,
        delivery: &Delivery,
        due_again_at: Timestamp,
    ) -> Result<()> {
        sqlx::query("UPDATE delivery SET due_at = ? WHERE run_id = ? AND turn = ?")
            .bind(due(due_again_at))
            .bind(delivery.run.to_string())
            .bind(delivery.turn.unwrap_or(0))
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("deferring what run {} said", delivery.run))?;

        Ok(())
    }
}

pub(crate) async fn event_with_id(
    connection: &mut SqliteConnection,
    id: EventRecordId,
) -> Result<Event> {
    let row = sqlx::query(
        "SELECT record_id, organization_id, integration_id, id, source, specversion, type,
                subject, time, data, recorded_at
         FROM event
         WHERE record_id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(&mut *connection)
    .await?
    .ok_or_else(|| Declined::Missing(format!("no event {id}")))?;

    event(&row)
}

async fn insert_event(
    connection: &mut SqliteConnection,
    organization: OrganizationId,
    integration: Option<IntegrationId>,
    occurrence: &Occurrence,
    data: &str,
) -> Result<Recorded> {
    let recorded = sqlx::query(
        "INSERT INTO event
             (record_id, organization_id, integration_id, id, source, specversion, type,
              subject, time, data, recorded_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (organization_id, source, id) DO NOTHING",
    )
    .bind(EventRecordId::generate().to_string())
    .bind(organization.to_string())
    .bind(integration.map(|integration| integration.to_string()))
    .bind(&occurrence.id)
    .bind(&occurrence.source)
    .bind(&occurrence.specversion)
    .bind(&occurrence.r#type)
    .bind(occurrence.subject.as_deref())
    .bind(occurrence.time.to_string())
    .bind(data)
    .bind(Timestamp::now().to_string())
    .execute(connection)
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

/// What a sealed signing secret is authenticated against, so one moved to another
/// integration's row no longer opens.
fn bound_to(integration: IntegrationId) -> String {
    format!("integration/{integration}")
}

fn integration(row: &SqliteRow) -> Result<Integration> {
    let mut carries = Vec::new();
    if row.get::<bool, _>("inbound") {
        carries.push(Direction::Inbound);
    }
    if row.get::<bool, _>("outbound") {
        carries.push(Direction::Outbound);
    }

    let connection = match row.get::<String, _>("kind").parse()? {
        IntegrationKind::Github => Connection::Github(GithubConnection {
            repository: row.get("repository"),
            api: row.get("api"),
            credential: Token::held(row.get("credential")),
            interval: SignedDuration::from_millis(row.get("interval_ms")),
            signed: row.get("signed"),
        }),
        IntegrationKind::Webhook => Connection::Webhook,
    };

    Ok(Integration {
        id: row.get::<String, _>("id").parse()?,
        organization: row.get::<String, _>("organization_id").parse()?,
        name: row.get("name"),
        connection,
        carries,
        poll_due_at: timestamp(row, "poll_due_at")?,
        polled_through: row.get("polled_through"),
        comments_polled_through: row.get("comments_polled_through"),
        last_event_refusal: event_refusal(row)?,
    })
}

fn event_refusal(row: &SqliteRow) -> Result<Option<EventRefusal>> {
    let Some(source) = row.get::<Option<String>, _>("last_event_refusal_source") else {
        return Ok(None);
    };

    Ok(Some(EventRefusal {
        source,
        id: row
            .get::<Option<String>, _>("last_event_refusal_id")
            .context("an event refusal has no producer id")?,
        bytes: usize::try_from(
            row.get::<Option<i64>, _>("last_event_refusal_bytes")
                .context("an event refusal has no byte size")?,
        )?,
        reason: row
            .get::<Option<String>, _>("last_event_refusal_reason")
            .context("an event refusal has no reason")?,
        observed_at: timestamp(row, "last_event_refusal_at")?
            .context("an event refusal has no observation time")?,
    }))
}

fn event(row: &SqliteRow) -> Result<Event> {
    Ok(Event {
        record_id: row.get::<String, _>("record_id").parse()?,
        organization: row.get::<String, _>("organization_id").parse()?,
        integration: row
            .get::<Option<String>, _>("integration_id")
            .map(|integration| integration.parse())
            .transpose()?,
        occurrence: Occurrence {
            id: row.get("id"),
            source: row.get("source"),
            specversion: row.get("specversion"),
            r#type: row.get("type"),
            subject: row.get("subject"),
            time: row.get::<String, _>("time").parse()?,
            data: serde_json::from_str(row.get("data"))?,
        },
        recorded_at: row.get::<String, _>("recorded_at").parse()?,
    })
}

fn delivery(row: &SqliteRow) -> Result<Delivery> {
    Ok(Delivery {
        run: row.get::<String, _>("run_id").parse()?,
        turn: match row.get::<i64, _>("turn") {
            0 => None,
            turn => Some(turn),
        },
        organization: row.get::<String, _>("organization_id").parse()?,
        integration: row.get::<String, _>("integration_id").parse()?,
        event: row.get::<String, _>("event_record_id").parse()?,
        subject: row.get("subject"),
        body: row.get("body"),
        attempted_at: timestamp(row, "attempted_at")?,
    })
}
