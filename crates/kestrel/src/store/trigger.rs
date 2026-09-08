use anyhow::{Context as _, Result};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use crate::domain::{
    Agent, Event, Organization, Session, Trigger, TriggerId, TriggerState, Workspace,
};
use crate::store::{agent, integration, organization, workspace};

macro_rules! triggers_where {
    ($tail:literal) => {
        concat!(
            "SELECT id, organization_id, name, repository, label, workspace_id, agent_id, state,
                    declared_at
             FROM trigger
             WHERE ",
            $tail
        )
    };
}

pub struct Triggers<'a> {
    connection: &'a mut SqliteConnection,
}

impl<'a> Triggers<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection) -> Self {
        Self { connection }
    }

    pub async fn declare(
        &mut self,
        organization: &Organization,
        name: &str,
        matching: (&str, &str),
        workspace: &Workspace,
        agent: &Agent,
    ) -> Result<Trigger> {
        let (repository, label) = matching;
        let trigger = Trigger {
            id: TriggerId::generate(),
            organization: organization.clone(),
            name: name.to_owned(),
            repository: repository.to_owned(),
            label: label.to_owned(),
            workspace: workspace.clone(),
            agent: agent.clone(),
            state: TriggerState::Enabled,
            declared_at: Timestamp::now(),
        };

        sqlx::query(
            "INSERT INTO trigger
                 (id, organization_id, name, repository, label, workspace_id, agent_id, state,
                  declared_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(trigger.id.to_string())
        .bind(organization.id.to_string())
        .bind(&trigger.name)
        .bind(&trigger.repository)
        .bind(&trigger.label)
        .bind(workspace.id.to_string())
        .bind(agent.id.to_string())
        .bind(trigger.state.as_str())
        .bind(trigger.declared_at.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("declaring the trigger {name}"))?;

        Ok(trigger)
    }

    pub async fn all(&mut self, organization: &Organization) -> Result<Vec<Trigger>> {
        let rows = sqlx::query(triggers_where!("organization_id = ? ORDER BY name"))
            .bind(organization.id.to_string())
            .fetch_all(&mut *self.connection)
            .await
            .context("reading an organization's triggers")?;

        let mut triggers = Vec::with_capacity(rows.len());
        for row in &rows {
            triggers.push(trigger(&mut *self.connection, row).await?);
        }

        Ok(triggers)
    }

    pub async fn named(&mut self, organization: &Organization, name: &str) -> Result<Trigger> {
        let row = sqlx::query(triggers_where!("organization_id = ? AND name = ?"))
            .bind(organization.id.to_string())
            .bind(name)
            .fetch_optional(&mut *self.connection)
            .await?
            .with_context(|| {
                format!(
                    "no trigger named {name} in the organization {}",
                    organization.name
                )
            })?;

        trigger(self.connection, &row).await
    }

    pub async fn set_state(&mut self, trigger: &Trigger, state: TriggerState) -> Result<Trigger> {
        sqlx::query("UPDATE trigger SET state = ? WHERE id = ?")
            .bind(state.as_str())
            .bind(trigger.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("changing whether the trigger {} fires", trigger.name))?;

        Ok(Trigger {
            state,
            ..trigger.clone()
        })
    }

    /// A Trigger fires at most once per Event, and the firing already recorded is what says
    /// so.
    pub async fn unfired_matches(
        &mut self,
        kind: &str,
        most: usize,
    ) -> Result<Vec<(Trigger, Event)>> {
        let rows = sqlx::query(
            "SELECT trigger.id AS trigger_id, event.id AS event_id
             FROM trigger
             JOIN event
               ON event.organization_id = trigger.organization_id
              AND event.repository = trigger.repository
              AND event.kind = ?
              AND event.label = trigger.label
             WHERE trigger.state = ?
               AND NOT EXISTS (
                   SELECT 1 FROM firing
                   WHERE firing.trigger_id = trigger.id AND firing.event_id = event.id
               )
             ORDER BY event.occurred_at, event.id
             LIMIT ?",
        )
        .bind(kind)
        .bind(TriggerState::Enabled.as_str())
        .bind(i64::try_from(most)?)
        .fetch_all(&mut *self.connection)
        .await
        .context("reading which events a trigger has yet to fire for")?;

        let mut matched = Vec::with_capacity(rows.len());
        for row in &rows {
            let trigger = with_id(
                &mut *self.connection,
                row.get::<String, _>("trigger_id").parse()?,
            )
            .await?;
            let event = integration::event_with_id(
                &mut *self.connection,
                row.get::<String, _>("event_id").parse()?,
            )
            .await?;
            matched.push((trigger, event));
        }

        Ok(matched)
    }

    pub async fn record_firing(
        &mut self,
        trigger: &Trigger,
        event: &Event,
        session: &Session,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO firing
                 (trigger_id, event_id, organization_id, session_id, fired_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(trigger.id.to_string())
        .bind(event.id.to_string())
        .bind(trigger.organization.id.to_string())
        .bind(session.id.to_string())
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| {
            format!(
                "recording that the trigger {} fired for the event {}",
                trigger.name, event.id
            )
        })?;

        Ok(())
    }
}

async fn with_id(connection: &mut SqliteConnection, id: TriggerId) -> Result<Trigger> {
    let row = sqlx::query(triggers_where!("id = ?"))
        .bind(id.to_string())
        .fetch_optional(&mut *connection)
        .await?
        .with_context(|| format!("no trigger {id}"))?;

    trigger(connection, &row).await
}

async fn trigger(connection: &mut SqliteConnection, row: &SqliteRow) -> Result<Trigger> {
    let organization =
        organization::with_id(connection, row.get::<String, _>("organization_id").parse()?).await?;
    let workspace = workspace::with_id(
        connection,
        &organization,
        row.get::<String, _>("workspace_id").parse()?,
    )
    .await?;
    let agent = agent::with_id(
        connection,
        &organization,
        row.get::<String, _>("agent_id").parse()?,
    )
    .await?;

    Ok(Trigger {
        id: row.get::<String, _>("id").parse()?,
        organization,
        name: row.get("name"),
        repository: row.get("repository"),
        label: row.get("label"),
        workspace,
        agent,
        state: row.get::<String, _>("state").parse()?,
        declared_at: row.get::<String, _>("declared_at").parse()?,
    })
}
