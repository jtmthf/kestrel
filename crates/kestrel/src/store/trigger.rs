use anyhow::{Context as _, Result};
use jiff::{SignedDuration, Timestamp};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::domain::{
    Agent, CorrelationMiss, DisableReason, Event, EventRecordId, Fires, Firing, FiringBudget,
    Organization, Session, Templates, Trigger, TriggerId, TriggerState, Workspace,
};
use crate::filter::{Attribute, Filter};
use crate::store::{agent, integration, organization, workspace};

macro_rules! triggers_where {
    ($tail:literal) => {
        concat!(
            "SELECT id, organization_id, name, filter, every_ms, due_at, brief, branch, correlation, on_miss, workspace_id,
                    agent_id, state, applied, declared_at
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

    #[expect(
        clippy::too_many_arguments,
        reason = "a trigger is what it is declared with"
    )]
    pub async fn declare(
        &mut self,
        organization: &Organization,
        name: &str,
        fires: &Fires,
        templates: &Templates,
        on_miss: Option<CorrelationMiss>,
        workspace: &Workspace,
        agent: &Agent,
        allows: &[Agent],
        applied: bool,
    ) -> Result<Trigger> {
        let trigger = Trigger {
            id: TriggerId::generate(),
            organization: organization.clone(),
            name: name.to_owned(),
            fires: fires.clone(),
            templates: templates.clone(),
            on_miss,
            workspace: workspace.clone(),
            agent: agent.clone(),
            allows: allows.to_vec(),
            state: TriggerState::Enabled,
            disabled_because: None,
            firing_budget: FiringBudget::default(),
            applied,
            declared_at: Timestamp::now(),
        };

        let (filter, every, due_at) = match fires {
            Fires::On(filter) => (Some(filter.to_json().to_string()), None, None),
            Fires::Every(every) => (
                None,
                Some(i64::try_from(every.as_millis())?),
                Some(trigger.declared_at.checked_add(*every)?.to_string()),
            ),
        };

        sqlx::query(
            "INSERT INTO trigger
                 (id, organization_id, name, filter, every_ms, due_at, brief, branch, correlation,
                  on_miss, workspace_id, agent_id, state, applied, enabled_at, declared_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(trigger.id.to_string())
        .bind(organization.id.to_string())
        .bind(&trigger.name)
        .bind(filter)
        .bind(every)
        .bind(due_at)
        .bind(templates.brief.to_string())
        .bind(templates.branch.as_ref().map(ToString::to_string))
        .bind(templates.correlation.as_ref().map(ToString::to_string))
        .bind(on_miss.map(CorrelationMiss::as_str))
        .bind(workspace.id.to_string())
        .bind(agent.id.to_string())
        .bind(trigger.state.as_str())
        .bind(applied)
        .bind(trigger.declared_at.to_string())
        .bind(trigger.declared_at.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("declaring the trigger {name}"))?;
        self.allow(&trigger, allows).await?;

        Ok(trigger)
    }

    /// Matches only what is recorded from now on, because the Events recorded under the old
    /// declaration were never judged against the new one.
    #[expect(
        clippy::too_many_arguments,
        reason = "a trigger is what it is declared with"
    )]
    pub async fn redeclare(
        &mut self,
        trigger: &Trigger,
        fires: &Fires,
        templates: &Templates,
        on_miss: Option<CorrelationMiss>,
        workspace: &Workspace,
        agent: &Agent,
        allows: &[Agent],
    ) -> Result<()> {
        let declared_at = Timestamp::now();
        let (filter, every, due_at) = match fires {
            Fires::On(filter) => (Some(filter.to_json().to_string()), None, None),
            Fires::Every(every) => (
                None,
                Some(i64::try_from(every.as_millis())?),
                Some(declared_at.checked_add(*every)?.to_string()),
            ),
        };

        sqlx::query(
            "UPDATE trigger
                SET filter = ?, every_ms = ?, due_at = ?, brief = ?, branch = ?, correlation = ?,
                    on_miss = ?, workspace_id = ?, agent_id = ?, applied = 1, declared_at = ?
              WHERE id = ?",
        )
        .bind(filter)
        .bind(every)
        .bind(due_at)
        .bind(templates.brief.to_string())
        .bind(templates.branch.as_ref().map(ToString::to_string))
        .bind(templates.correlation.as_ref().map(ToString::to_string))
        .bind(on_miss.map(CorrelationMiss::as_str))
        .bind(workspace.id.to_string())
        .bind(agent.id.to_string())
        .bind(declared_at.to_string())
        .bind(trigger.id.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("redeclaring the trigger {}", trigger.name))?;
        self.allow(trigger, allows).await?;

        Ok(())
    }

    async fn allow(&mut self, trigger: &Trigger, allows: &[Agent]) -> Result<()> {
        sqlx::query("DELETE FROM trigger_agent WHERE trigger_id = ?")
            .bind(trigger.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| {
                format!("forgetting the agents the trigger {} allows", trigger.name)
            })?;
        for agent in allows {
            sqlx::query(
                "INSERT INTO trigger_agent (trigger_id, organization_id, agent_id)
                 VALUES (?, ?, ?)
                 ON CONFLICT DO NOTHING",
            )
            .bind(trigger.id.to_string())
            .bind(trigger.organization.id.to_string())
            .bind(agent.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| {
                format!(
                    "allowing the trigger {} the agent {}",
                    trigger.name, agent.name
                )
            })?;
        }

        Ok(())
    }

    pub async fn adopt(&mut self, trigger: &Trigger) -> Result<()> {
        sqlx::query("UPDATE trigger SET applied = 1 WHERE id = ?")
            .bind(trigger.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("applying the trigger {}", trigger.name))?;

        Ok(())
    }

    pub async fn remove(&mut self, trigger: &Trigger) -> Result<()> {
        sqlx::query("DELETE FROM firing WHERE trigger_id = ?")
            .bind(trigger.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("forgetting the firings of the trigger {}", trigger.name))?;
        self.allow(trigger, &[]).await?;
        sqlx::query("DELETE FROM trigger WHERE id = ?")
            .bind(trigger.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("removing the trigger {}", trigger.name))?;

        Ok(())
    }

    pub async fn all(&mut self, organization: &Organization) -> Result<Vec<Trigger>> {
        let rows = sqlx::query(triggers_where!("organization_id = ? ORDER BY name"))
            .bind(organization.id.to_string())
            .fetch_all(&mut *self.connection)
            .await
            .context("reading an organization's triggers")?;

        self.triggers(&rows).await
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

    pub async fn due_at(&mut self, trigger: &Trigger) -> Result<Option<Timestamp>> {
        sqlx::query("SELECT due_at FROM trigger WHERE id = ?")
            .bind(trigger.id.to_string())
            .fetch_one(&mut *self.connection)
            .await
            .with_context(|| format!("reading when the trigger {} is next due", trigger.name))?
            .get::<Option<String>, _>("due_at")
            .map(|due| due.parse())
            .transpose()
            .map_err(Into::into)
    }

    /// A disabled Trigger's schedule does not elapse, so enabling it again is not a backlog.
    pub async fn schedules_due(&mut self, at: Timestamp) -> Result<Vec<(Trigger, Timestamp)>> {
        let rows = sqlx::query(triggers_where!(
            "state = ? AND due_at <= ? ORDER BY due_at, id"
        ))
        .bind(TriggerState::Enabled.as_str())
        .bind(at.to_string())
        .fetch_all(&mut *self.connection)
        .await
        .context("reading which triggers' schedules are due")?;

        let mut due = Vec::with_capacity(rows.len());
        for row in &rows {
            let due_at = row.get::<String, _>("due_at").parse()?;
            due.push((trigger(&mut *self.connection, row).await?, due_at));
        }

        Ok(due)
    }

    pub async fn due_again(&mut self, trigger: &Trigger, at: Timestamp) -> Result<()> {
        sqlx::query("UPDATE trigger SET due_at = ? WHERE id = ?")
            .bind(at.to_string())
            .bind(trigger.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("setting when the trigger {} is next due", trigger.name))?;

        Ok(())
    }

    pub async fn set_state(&mut self, trigger: &Trigger, state: TriggerState) -> Result<Trigger> {
        let enabled = TriggerState::Enabled.as_str();
        sqlx::query(
            "UPDATE trigger
             SET enabled_at = CASE WHEN ? = ? AND state <> ? THEN ? ELSE enabled_at END,
                 state = ?
             WHERE id = ?",
        )
        .bind(state.as_str())
        .bind(enabled)
        .bind(enabled)
        .bind(Timestamp::now().to_string())
        .bind(state.as_str())
        .bind(trigger.id.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("changing whether the trigger {} fires", trigger.name))?;

        let changed = Trigger {
            state,
            ..trigger.clone()
        };
        Ok(Trigger {
            disabled_because: disabled_because(&changed, changed.state.clone()),
            ..changed
        })
    }

    pub async fn disabled_because(&mut self, trigger: &Trigger) -> Result<Option<String>> {
        let state = sqlx::query("SELECT state FROM trigger WHERE id = ?")
            .bind(trigger.id.to_string())
            .fetch_one(&mut *self.connection)
            .await
            .with_context(|| format!("reading whether trigger {} fires", trigger.name))?
            .get::<String, _>("state")
            .parse::<TriggerState>()?;

        Ok(disabled_because(trigger, state))
    }

    pub async fn firing_budget_is_exhausted(
        &mut self,
        trigger: &Trigger,
        at: Timestamp,
    ) -> Result<bool> {
        let enabled_at: Timestamp = sqlx::query("SELECT enabled_at FROM trigger WHERE id = ?")
            .bind(trigger.id.to_string())
            .fetch_one(&mut *self.connection)
            .await
            .with_context(|| format!("reading when trigger {} was enabled", trigger.name))?
            .get::<String, _>("enabled_at")
            .parse()?;
        // Firings before an operator re-enabled the trigger would disable it again at once.
        let start = at
            .checked_sub(trigger.firing_budget.window)
            .context("placing the start of a trigger's firing budget window")?
            .max(enabled_at);
        let firings: i64 = sqlx::query(
            "SELECT COUNT(*) AS firings
             FROM firing
             WHERE trigger_id = ? AND fired_at >= ?",
        )
        .bind(trigger.id.to_string())
        .bind(start.to_string())
        .fetch_one(&mut *self.connection)
        .await
        .with_context(|| format!("counting recent firings of trigger {}", trigger.name))?
        .get("firings");

        Ok(firings >= i64::try_from(trigger.firing_budget.limit.get())?)
    }

    pub async fn matches(&mut self, trigger: &Trigger, event: &Event) -> Result<bool> {
        let mut query = QueryBuilder::<Sqlite>::new("SELECT ");
        matching(&mut query, trigger);
        query
            .push(" AS matched FROM event WHERE record_id = ")
            .push_bind(event.record_id.to_string());

        let row = query
            .build()
            .fetch_one(&mut *self.connection)
            .await
            .with_context(|| {
                format!(
                    "testing the trigger {} against the event {}",
                    trigger.name, event.record_id
                )
            })?;

        Ok(row.get("matched"))
    }

    /// A Trigger fires at most once per Event, and the firing already recorded is what says
    /// so. An Event recorded before the Trigger was declared is never matched at all:
    /// declaring a Trigger is not how a repository's existing history gets worked.
    pub async fn unfired_matches(&mut self, most: usize) -> Result<Vec<(Trigger, Event)>> {
        let rows = sqlx::query(triggers_where!("state = ? ORDER BY declared_at, id"))
            .bind(TriggerState::Enabled.as_str())
            .fetch_all(&mut *self.connection)
            .await
            .context("reading the triggers that fire")?;

        let mut matched = Vec::new();
        for trigger in self.triggers(&rows).await? {
            let remaining = most - matched.len();
            if remaining == 0 {
                break;
            }

            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT record_id FROM event
                 WHERE organization_id = ",
            );
            query
                .push_bind(trigger.organization.id.to_string())
                .push(" AND recorded_at >= ")
                .push_bind(trigger.declared_at.to_string())
                .push(
                    " AND NOT EXISTS (
                         SELECT 1 FROM firing
                          WHERE firing.event_record_id = event.record_id
                            AND firing.trigger_id = ",
                )
                .push_bind(trigger.id.to_string())
                .push(") AND ");
            matching(&mut query, &trigger);
            query
                .push(" ORDER BY time, record_id LIMIT ")
                .push_bind(i64::try_from(remaining)?);

            let events = query
                .build()
                .fetch_all(&mut *self.connection)
                .await
                .with_context(|| {
                    format!(
                        "reading which events the trigger {} has yet to fire for",
                        trigger.name
                    )
                })?;

            for row in &events {
                let event = integration::event_with_id(
                    &mut *self.connection,
                    row.get::<String, _>("record_id").parse()?,
                )
                .await?;
                matched.push((trigger.clone(), event));
            }
        }

        Ok(matched)
    }

    pub async fn firings_of(&mut self, event: EventRecordId) -> Result<Vec<Firing>> {
        sqlx::query(
            "SELECT trigger.name, firing.outcome, firing.session_id, firing.failure
             FROM firing
             JOIN trigger ON trigger.id = firing.trigger_id
             WHERE firing.event_record_id = ?
             ORDER BY firing.fired_at, trigger.name",
        )
        .bind(event.to_string())
        .fetch_all(&mut *self.connection)
        .await
        .with_context(|| format!("reading what the event {event} fired"))?
        .iter()
        .map(|row| {
            Ok(Firing {
                trigger: row.get("name"),
                outcome: row.get("outcome"),
                session: row
                    .get::<Option<String>, _>("session_id")
                    .map(|session| session.parse())
                    .transpose()?,
                failure: row.get("failure"),
            })
        })
        .collect()
    }

    pub async fn record_opened_firing(
        &mut self,
        trigger: &Trigger,
        event: &Event,
        session: &Session,
    ) -> Result<()> {
        self.record(trigger, event, Some(session), "opened", None)
            .await
    }

    pub async fn record_fed_firing(
        &mut self,
        trigger: &Trigger,
        event: &Event,
        session: &Session,
    ) -> Result<()> {
        self.record(trigger, event, Some(session), "fed", None)
            .await
    }

    pub async fn record_ignored_firing(&mut self, trigger: &Trigger, event: &Event) -> Result<()> {
        self.record(trigger, event, None, "ignored", None).await
    }

    pub async fn record_failed_firing(
        &mut self,
        trigger: &Trigger,
        event: &Event,
        because: &str,
    ) -> Result<()> {
        self.record(trigger, event, None, "failed", Some(because))
            .await
    }

    async fn record(
        &mut self,
        trigger: &Trigger,
        event: &Event,
        session: Option<&Session>,
        outcome: &str,
        failure: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO firing
                 (trigger_id, event_record_id, organization_id, session_id, outcome, failure, fired_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(trigger.id.to_string())
        .bind(event.record_id.to_string())
        .bind(trigger.organization.id.to_string())
        .bind(session.map(|session| session.id.to_string()))
        .bind(outcome)
        .bind(failure)
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| {
            format!(
                "recording that the trigger {} fired for the event {}",
                trigger.name, event.record_id
            )
        })?;

        Ok(())
    }

    async fn triggers(&mut self, rows: &[SqliteRow]) -> Result<Vec<Trigger>> {
        let mut triggers = Vec::with_capacity(rows.len());
        for row in rows {
            triggers.push(trigger(&mut *self.connection, row).await?);
        }

        Ok(triggers)
    }
}

fn matching(query: &mut QueryBuilder<Sqlite>, trigger: &Trigger) {
    query.push("(");
    predicate(query, &trigger.filter());
    // A webhook can name any source and type, so only kestrel's own minting elapses a schedule.
    if matches!(trigger.fires, Fires::Every(_)) {
        query.push(" AND event.integration_id IS NULL");
    }
    query
        .push(" AND event.type <> ")
        .push_bind(crate::trigger::DISPATCHED)
        .push(")");
}

/// Every comparison is coalesced to false, because an attribute an Event lacks is NULL and
/// `NOT NULL` would leave `not` matching nothing rather than everything.
fn predicate(query: &mut QueryBuilder<Sqlite>, filter: &Filter) {
    match filter {
        Filter::Exact(attribute, value) => {
            query.push("COALESCE(");
            operand(query, attribute);
            query.push(" = ").push_bind(value.clone()).push(", 0)");
        }
        // LIKE would fold ASCII case and read `%` and `_` in the value as wildcards.
        Filter::Prefix(attribute, value) => {
            query.push("COALESCE(substr(");
            operand(query, attribute);
            query
                .push(", 1, length(")
                .push_bind(value.clone())
                .push(")) = ")
                .push_bind(value.clone())
                .push(", 0)");
        }
        Filter::Suffix(attribute, value) => {
            query.push("COALESCE(substr(");
            operand(query, attribute);
            query
                .push(", -length(")
                .push_bind(value.clone())
                .push(")) = ")
                .push_bind(value.clone())
                .push(", 0)");
        }
        Filter::All(filters) => joined(query, filters, " AND "),
        Filter::Any(filters) => joined(query, filters, " OR "),
        Filter::Not(filter) => {
            query.push("NOT (");
            predicate(query, filter);
            query.push(")");
        }
    }
}

fn joined(query: &mut QueryBuilder<Sqlite>, filters: &[Filter], by: &str) {
    query.push("(");
    for (at, filter) in filters.iter().enumerate() {
        if at > 0 {
            query.push(by);
        }
        predicate(query, filter);
    }
    query.push(")");
}

/// A string in `data` compares as itself, and an integer or a boolean as the text it is
/// written as; anything else, and a path that leads nowhere, compares as absent.
fn operand(query: &mut QueryBuilder<Sqlite>, attribute: &Attribute) {
    let column = match attribute {
        Attribute::Id => "event.id",
        Attribute::Source => "event.source",
        Attribute::Specversion => "event.specversion",
        Attribute::Type => "event.type",
        Attribute::Subject => "event.subject",
        Attribute::Time => "event.time",
        Attribute::Data(path) => {
            let path = format!(
                "${}",
                path.iter()
                    .map(|key| format!(".\"{key}\""))
                    .collect::<String>()
            );
            query
                .push("CASE json_type(event.data, ")
                .push_bind(path.clone())
                .push(") WHEN 'text' THEN json_extract(event.data, ")
                .push_bind(path.clone())
                .push(") WHEN 'integer' THEN CAST(json_extract(event.data, ")
                .push_bind(path)
                .push(") AS TEXT) WHEN 'true' THEN 'true' WHEN 'false' THEN 'false' END");
            return;
        }
    };
    query.push(column);
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
    let mut allows = Vec::new();
    for allowed in sqlx::query(
        "SELECT agent.id FROM trigger_agent
         JOIN agent ON agent.id = trigger_agent.agent_id
         WHERE trigger_agent.trigger_id = ?
         ORDER BY agent.name",
    )
    .bind(row.get::<String, _>("id"))
    .fetch_all(&mut *connection)
    .await?
    {
        allows.push(
            agent::with_id(
                connection,
                &organization,
                allowed.get::<String, _>("id").parse()?,
            )
            .await?,
        );
    }

    let state: TriggerState = row.get::<String, _>("state").parse()?;

    let trigger = Trigger {
        id: row.get::<String, _>("id").parse()?,
        organization,
        name: row.get("name"),
        fires: match (
            row.get::<Option<String>, _>("filter"),
            row.get::<Option<i64>, _>("every_ms"),
        ) {
            (Some(filter), None) => Fires::On(filter.parse()?),
            (None, Some(every)) => Fires::Every(SignedDuration::from_millis(every)),
            _ => anyhow::bail!("a trigger fires on a filter or a schedule, and not both"),
        },
        templates: Templates {
            brief: row.get::<String, _>("brief").parse()?,
            branch: row
                .get::<Option<String>, _>("branch")
                .map(|branch| branch.parse())
                .transpose()?,
            correlation: row
                .get::<Option<String>, _>("correlation")
                .map(|correlation| correlation.parse())
                .transpose()?,
        },
        on_miss: row
            .get::<Option<String>, _>("on_miss")
            .map(|miss| miss.parse())
            .transpose()?,
        workspace,
        agent,
        allows,
        state: state.clone(),
        disabled_because: None,
        firing_budget: FiringBudget::default(),
        applied: row.get("applied"),
        declared_at: row.get::<String, _>("declared_at").parse()?,
    };

    Ok(Trigger {
        disabled_because: disabled_because(&trigger, state),
        ..trigger
    })
}

fn disabled_because(trigger: &Trigger, state: TriggerState) -> Option<String> {
    match state {
        TriggerState::Enabled => None,
        TriggerState::Disabled(DisableReason::Operator) => {
            Some("disabled by an operator".to_owned())
        }
        TriggerState::Disabled(DisableReason::FiringBudget) => {
            Some(trigger.firing_budget_exhausted_because())
        }
    }
}
