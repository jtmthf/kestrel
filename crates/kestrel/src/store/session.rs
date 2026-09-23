use anyhow::{Context as _, Result};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use crate::domain::{
    Agent, Checkout, Connected, Cost, Event, Exit, Organization, Run, RunId, RunState, Session,
    SessionId, SessionState, SubscriptionProfile, Turn, Usage, Workspace,
};
use crate::instance::Observed;
use crate::link::credential::Credential;
use crate::link::{Instruction, SentInstruction};
use crate::reference::{self, Candidate, Reference};
use crate::store::{agent, due, organization, profile, timestamp, workspace};

macro_rules! runs_where {
    ($tail:literal) => {
        concat!(
            "SELECT id, name, organization_id, session_id, state, waiting_for, exit, exit_because, instance, supervisor,
                    enqueued_at, started_at, ended_at, lease_expires_at, connected_at,
                    supervisor_version, model, worked_model, context_used, context_size, cost_amount,
                    cost_currency
             FROM run
             WHERE ",
            $tail
        )
    };
}

/// Prompted at least once and every prompt answered, over a `run AS r`. A Run not yet prompted
/// is still getting to its first turn.
macro_rules! between_turns {
    () => {
        "EXISTS (SELECT 1 FROM turn AS t WHERE t.run_id = r.id)
         AND NOT EXISTS (SELECT 1 FROM turn AS t WHERE t.run_id = r.id AND t.answered_at IS NULL)"
    };
}

/// What became of a report the link was handed: the next in the supervisor's sequence, one
/// taken already — where a replay after an answer that never arrived lands — or one that
/// skips a report the Run has yet to make, which would leave a gap nothing fills.
pub enum Taken {
    Next,
    Again,
    Skipped,
}

/// A Session's Instance, and what its checkout was last observed to hold: `None` until a Run on it
/// reports, and forgotten whenever another Run starts on it.
pub struct Kept {
    pub session: SessionId,
    pub instance: String,
    pub observed: Option<Vec<Observed>>,
}

pub struct PendingMessage {
    pub participant: String,
    pub body: String,
}

pub struct Opening<'a> {
    pub organization: &'a Organization,
    pub workspace: &'a Workspace,
    pub agent: &'a Agent,
    pub profile: Option<&'a SubscriptionProfile>,
    /// None declares the Session a branch of its own.
    pub branch: Option<&'a str>,
    pub correlation: Option<&'a str>,
    pub continues: Option<&'a Session>,
    pub started_by: Option<&'a Event>,
}

pub struct Sessions<'a> {
    connection: &'a mut SqliteConnection,
}

impl<'a> Sessions<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection) -> Self {
        Self { connection }
    }

    pub async fn open(&mut self, opening: Opening<'_>) -> Result<Session> {
        let opened_at = Timestamp::now();
        let id = SessionId::generate();
        let session = loop {
            let session = Session {
                id,
                name: generated_name(),
                organization: opening.organization.clone(),
                workspace: opening.workspace.clone(),
                agent: opening.agent.clone(),
                profile: opening.profile.cloned(),
                checkout: Checkout {
                    repositories: opening.workspace.repositories.clone(),
                    base: opening.workspace.branch.clone(),
                    branch: opening
                        .branch
                        .map_or_else(|| format!("kestrel/{id}"), ToOwned::to_owned),
                },
                correlation: opening.correlation.map(ToOwned::to_owned),
                state: SessionState::Open,
                opened_at,
                last_active_at: opened_at,
                sealed_at: None,
                continues: opening.continues.map(|sealed| sealed.id),
                started_by: opening.started_by.map(|event| event.record_id),
            };

            let inserted = sqlx::query(
                "INSERT INTO session
                     (id, name, organization_id, workspace_id, agent_id, runtime, model,
                      subscription_profile_id, base, branch, correlation, state, opened_at,
                      last_active_at, continues, event_record_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (organization_id, name) DO NOTHING",
            )
            .bind(session.id.to_string())
            .bind(&session.name)
            .bind(session.organization.id.to_string())
            .bind(session.workspace.id.to_string())
            .bind(session.agent.id.to_string())
            .bind(&session.agent.runtime)
            .bind(&session.agent.model)
            .bind(
                session
                    .profile
                    .as_ref()
                    .map(|profile| profile.id.to_string()),
            )
            .bind(&session.checkout.base)
            .bind(&session.checkout.branch)
            .bind(&session.correlation)
            .bind(session.state.as_str())
            .bind(session.opened_at.to_string())
            .bind(due(session.last_active_at))
            .bind(session.continues.map(|sealed| sealed.to_string()))
            .bind(session.started_by.map(|event| event.to_string()))
            .execute(&mut *self.connection)
            .await
            .context("opening a session")?;

            if inserted.rows_affected() == 1 {
                break session;
            }
        };

        for (position, url) in session.checkout.repositories.iter().enumerate() {
            sqlx::query(
                "INSERT INTO session_repository (session_id, organization_id, position, url)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(session.id.to_string())
            .bind(session.organization.id.to_string())
            .bind(i64::try_from(position)?)
            .bind(url)
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("fixing the repository {url} to the session {id}"))?;
        }

        Ok(session)
    }

    pub async fn holding_correlation(
        &mut self,
        organization: &Organization,
        correlation: &str,
    ) -> Result<Option<SessionId>> {
        sqlx::query(
            "SELECT id FROM session WHERE organization_id = ? AND state = ? AND correlation = ?",
        )
        .bind(organization.id.to_string())
        .bind(SessionState::Open.as_str())
        .bind(correlation)
        .fetch_optional(&mut *self.connection)
        .await
        .with_context(|| format!("reading which open session holds the correlation {correlation}"))?
        .map(|row| Ok(row.get::<String, _>("id").parse()?))
        .transpose()
    }

    pub async fn seal(&mut self, session: &Session) -> Result<Timestamp> {
        let sealed_at = Timestamp::now();

        sqlx::query("UPDATE session SET state = ?, sealed_at = ? WHERE id = ?")
            .bind(SessionState::Sealed.as_str())
            .bind(sealed_at.to_string())
            .bind(session.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("sealing the session {}", session.id))?;

        Ok(sealed_at)
    }

    pub async fn record_active(&mut self, session: SessionId, at: Timestamp) -> Result<()> {
        sqlx::query("UPDATE session SET last_active_at = ? WHERE id = ?")
            .bind(due(at))
            .bind(session.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording the session {session} active"))?;

        Ok(())
    }

    pub async fn idle(&mut self, before: Timestamp) -> Result<Vec<Session>> {
        let ids = sqlx::query(
            "SELECT id
             FROM session
             WHERE state = ? AND last_active_at <= ?
             ORDER BY last_active_at, id",
        )
        .bind(SessionState::Open.as_str())
        .bind(due(before))
        .fetch_all(&mut *self.connection)
        .await
        .context("sweeping idle sessions")?
        .iter()
        .map(|row| Ok(row.get::<String, _>("id").parse()?))
        .collect::<Result<Vec<SessionId>>>()?;

        let mut idle = Vec::with_capacity(ids.len());
        for id in ids {
            idle.push(read(&mut *self.connection, id).await?);
        }

        Ok(idle)
    }

    pub async fn get(&mut self, id: SessionId) -> Result<Session> {
        read(self.connection, id).await
    }

    pub async fn find(&mut self, id: SessionId) -> Result<Option<Session>> {
        find(self.connection, id).await
    }

    /// Resolves what an operator typed to exactly one Session in the organization, refusing
    /// when nothing matched or when several did.
    pub async fn resolved(&mut self, organization: &Organization, typed: &str) -> Result<Session> {
        let reference = Reference::read(typed);

        if reference.is_latest() {
            let latest = sqlx::query(
                "SELECT id FROM session
                 WHERE organization_id = ?
                 ORDER BY opened_at DESC, id DESC
                 LIMIT 1",
            )
            .bind(organization.id.to_string())
            .fetch_optional(&mut *self.connection)
            .await
            .context("reading the most recent session")?;
            let Some(latest) = latest else {
                return Err(reference::missing("session", &organization.name, typed));
            };

            return read(
                &mut *self.connection,
                latest.get::<String, _>("id").parse()?,
            )
            .await;
        }

        let given = reference
            .given()
            .expect("a reference that is not the latest names one");
        let named = sqlx::query("SELECT id FROM session WHERE organization_id = ? AND name = ?")
            .bind(organization.id.to_string())
            .bind(given)
            .fetch_optional(&mut *self.connection)
            .await
            .context("reading a session by its generated name")?;
        if let Some(named) = named {
            return read(&mut *self.connection, named.get::<String, _>("id").parse()?).await;
        }

        if let Some(prefix) = reference.prefix() {
            let matched = sqlx::query(
                "SELECT id, name FROM session
                 WHERE organization_id = ? AND REPLACE(LOWER(id), '-', '') LIKE ? || '%'
                 ORDER BY name, id",
            )
            .bind(organization.id.to_string())
            .bind(prefix)
            .fetch_all(&mut *self.connection)
            .await
            .context("reading sessions by identifier prefix")?
            .iter()
            .map(candidate)
            .collect::<Vec<_>>();

            match matched.as_slice() {
                [] => {}
                [only] => return read(&mut *self.connection, only.id.parse()?).await,
                _ => {
                    return Err(reference::ambiguous(
                        "session",
                        &organization.name,
                        given,
                        &matched,
                    ));
                }
            }
        }

        Err(reference::missing("session", &organization.name, given))
    }

    /// A Run on the same terms as a Session.
    pub async fn resolved_run(&mut self, organization: &Organization, typed: &str) -> Result<Run> {
        let reference = Reference::read(typed);

        if reference.is_latest() {
            let latest = sqlx::query(runs_where!(
                "organization_id = ?
                 ORDER BY enqueued_at DESC, id DESC
                 LIMIT 1"
            ))
            .bind(organization.id.to_string())
            .fetch_optional(&mut *self.connection)
            .await
            .context("reading the most recent run")?;
            let Some(latest) = latest else {
                return Err(reference::missing("run", &organization.name, typed));
            };

            return run(&latest);
        }

        let given = reference
            .given()
            .expect("a reference that is not the latest names one");
        let named = sqlx::query(runs_where!("organization_id = ? AND name = ?"))
            .bind(organization.id.to_string())
            .bind(given)
            .fetch_optional(&mut *self.connection)
            .await
            .context("reading a run by its generated name")?;
        if let Some(named) = named {
            return run(&named);
        }

        if let Some(prefix) = reference.prefix() {
            let matched = sqlx::query(
                "SELECT id, name FROM run
                 WHERE organization_id = ? AND REPLACE(LOWER(id), '-', '') LIKE ? || '%'
                 ORDER BY name, id",
            )
            .bind(organization.id.to_string())
            .bind(prefix)
            .fetch_all(&mut *self.connection)
            .await
            .context("reading runs by identifier prefix")?
            .iter()
            .map(candidate)
            .collect::<Vec<_>>();

            match matched.as_slice() {
                [] => {}
                [only] => return self.run(only.id.parse()?).await,
                _ => {
                    return Err(reference::ambiguous(
                        "run",
                        &organization.name,
                        given,
                        &matched,
                    ));
                }
            }
        }

        Err(reference::missing("run", &organization.name, given))
    }

    pub async fn all(&mut self, organization: &Organization) -> Result<Vec<Session>> {
        let ids =
            sqlx::query("SELECT id FROM session WHERE organization_id = ? ORDER BY opened_at, id")
                .bind(organization.id.to_string())
                .fetch_all(&mut *self.connection)
                .await
                .context("reading an organization's sessions")?
                .iter()
                .map(|row| Ok(row.get::<String, _>("id").parse()?))
                .collect::<Result<Vec<SessionId>>>()?;

        let mut sessions = Vec::with_capacity(ids.len());
        for id in ids {
            sessions.push(read(&mut *self.connection, id).await?);
        }

        Ok(sessions)
    }

    /// Read on its own rather than with the Session: every path that reports a Run reads one,
    /// and none of them looks at what continues it.
    pub async fn continuations(&mut self, sealed: SessionId) -> Result<Vec<SessionId>> {
        sqlx::query("SELECT id FROM session WHERE continues = ? ORDER BY opened_at, id")
            .bind(sealed.to_string())
            .fetch_all(&mut *self.connection)
            .await
            .with_context(|| format!("reading what continues the session {sealed}"))?
            .iter()
            .map(|row| Ok(row.get::<String, _>("id").parse()?))
            .collect()
    }

    /// The slot is taken from the moment work is enqueued rather than from the moment it is
    /// dispatched: two Runs queued in one Session would otherwise both be handed out. A Run
    /// that turned out unreachable never occupied it any longer than one that ended does.
    pub async fn run_holding_the_slot(&mut self, session: &Session) -> Result<Option<RunId>> {
        let holding = sqlx::query(
            "SELECT id
             FROM run
             WHERE session_id = ?
               AND (state NOT IN (?, ?) OR supervisor_state = 'present')
             ORDER BY enqueued_at, id
             LIMIT 1",
        )
        .bind(session.id.to_string())
        .bind(RunState::Ended.as_str())
        .bind(RunState::Unreachable.as_str())
        .fetch_optional(&mut *self.connection)
        .await
        .with_context(|| format!("reading what run the session {} has", session.id))?;

        holding
            .map(|row| Ok(row.get::<String, _>("id").parse()?))
            .transpose()
    }

    pub async fn sealed_holding_correlation(
        &mut self,
        organization: &Organization,
        correlation: &str,
    ) -> Result<Option<Session>> {
        let sealed = sqlx::query(
            "SELECT id FROM session
             WHERE organization_id = ? AND state = ? AND correlation = ?
             ORDER BY sealed_at DESC, id DESC
             LIMIT 1",
        )
        .bind(organization.id.to_string())
        .bind(SessionState::Sealed.as_str())
        .bind(correlation)
        .fetch_optional(&mut *self.connection)
        .await
        .with_context(|| {
            format!("reading which sealed session held the correlation {correlation}")
        })?;

        let Some(sealed) = sealed else {
            return Ok(None);
        };
        let id = sealed.get::<String, _>("id").parse()?;

        Ok(Some(read(&mut *self.connection, id).await?))
    }

    pub async fn enqueue_run(&mut self, session: &Session, model: Option<&str>) -> Result<Run> {
        let run = loop {
            let run = Run {
                id: RunId::generate(),
                name: generated_name(),
                organization: session.organization.id,
                session: session.id,
                state: RunState::Queued,
                waiting_for: None,
                exit: None,
                instance: None,
                supervisor: None,
                model: model.map(str::to_owned),
                worked_model: None,
                enqueued_at: Timestamp::now(),
                started_at: None,
                ended_at: None,
                lease_expires_at: None,
                connected: None,
                usage: None,
            };

            let inserted = sqlx::query(
                "INSERT INTO run (id, name, organization_id, session_id, state, model, enqueued_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (organization_id, name) DO NOTHING",
            )
            .bind(run.id.to_string())
            .bind(&run.name)
            .bind(run.organization.to_string())
            .bind(run.session.to_string())
            .bind(run.state.as_str())
            .bind(&run.model)
            .bind(due(run.enqueued_at))
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("enqueueing a run in the session {}", session.id))?;

            if inserted.rows_affected() == 1 {
                break run;
            }
        };

        self.record_active(session.id, run.enqueued_at).await?;

        Ok(run)
    }

    pub async fn add_pending_message(
        &mut self,
        session: &Session,
        participant: &str,
        body: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO pending_message (
                 session_id, organization_id, seq, participant, body, received_at
             )
             SELECT ?, ?, COALESCE(MAX(seq), 0) + 1, ?, ?, ?
             FROM pending_message
             WHERE session_id = ?",
        )
        .bind(session.id.to_string())
        .bind(session.organization.id.to_string())
        .bind(participant)
        .bind(body)
        .bind(due(Timestamp::now()))
        .bind(session.id.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("holding a pending message in the session {}", session.id))?;

        Ok(())
    }

    pub async fn take_pending_messages(
        &mut self,
        session: &Session,
    ) -> Result<Vec<PendingMessage>> {
        let rows = sqlx::query(
            "DELETE FROM pending_message
             WHERE session_id = ?
             RETURNING participant, body, seq",
        )
        .bind(session.id.to_string())
        .fetch_all(&mut *self.connection)
        .await
        .with_context(|| format!("taking pending messages from the session {}", session.id))?;

        let mut messages = rows
            .iter()
            .map(|row| {
                (
                    row.get::<i64, _>("seq"),
                    PendingMessage {
                        participant: row.get("participant"),
                        body: row.get("body"),
                    },
                )
            })
            .collect::<Vec<_>>();
        messages.sort_by_key(|(seq, _)| *seq);

        Ok(messages.into_iter().map(|(_, message)| message).collect())
    }

    pub async fn has_pending_messages(&mut self, session: &Session) -> Result<bool> {
        let row = sqlx::query(
            "SELECT EXISTS (
                 SELECT 1 FROM pending_message WHERE session_id = ?
             ) AS pending",
        )
        .bind(session.id.to_string())
        .fetch_one(&mut *self.connection)
        .await?;

        Ok(row.get("pending"))
    }

    pub async fn declare_blocked(&mut self, run: &Run, blocker: &Run) -> Result<()> {
        if run.organization != blocker.organization {
            anyhow::bail!(
                "the run {} and the run {} it is blocked on are in different organizations",
                run.id,
                blocker.id
            );
        }

        sqlx::query(
            "INSERT INTO run_dependency (run_id, blocker_id, organization_id)
             VALUES (?, ?, ?)",
        )
        .bind(run.id.to_string())
        .bind(blocker.id.to_string())
        .bind(run.organization.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("declaring the run {} blocked on {}", run.id, blocker.id))?;

        Ok(())
    }

    /// Every queued Run still waiting on this one as a blocker. Tolerance defaults to
    /// all-must-succeed, so any one of them is enough to name a dependent whose tolerance a
    /// blocker that just ended without succeeding can no longer meet.
    pub async fn dependents_of(&mut self, blocker: RunId) -> Result<Vec<Run>> {
        sqlx::query(runs_where!(
            "state = ?
               AND id IN (SELECT run_id FROM run_dependency WHERE blocker_id = ?)
             ORDER BY enqueued_at, id"
        ))
        .bind(RunState::Queued.as_str())
        .bind(blocker.to_string())
        .fetch_all(&mut *self.connection)
        .await
        .with_context(|| format!("reading what is still waiting on the run {blocker}"))?
        .iter()
        .map(run)
        .collect()
    }

    /// A queued Run whose declared tolerance can no longer be met: terminal like an ended Run,
    /// but never claimed and never carrying an exit status, because nothing failed. `false`
    /// when the Run was no longer queued, so a claimant that got there first stands.
    pub async fn mark_unreachable(&mut self, run: &Run) -> Result<bool> {
        let marked = sqlx::query(
            "UPDATE run SET state = ?, waiting_for = NULL, ended_at = ?
                 WHERE id = ? AND state = ?",
        )
        .bind(RunState::Unreachable.as_str())
        .bind(Timestamp::now().to_string())
        .bind(run.id.to_string())
        .bind(RunState::Queued.as_str())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("marking the run {} unreachable", run.id))?;

        Ok(marked.rows_affected() > 0)
    }

    pub async fn claimable_runs(
        &mut self,
        serialized: &[String],
        enqueued_before: Option<Timestamp>,
    ) -> Result<Vec<Run>> {
        let ids = sqlx::query(
            "SELECT r.id
             FROM run AS r
             WHERE r.state = ?
               AND NOT EXISTS (
                   SELECT 1
                   FROM run_dependency AS d
                   JOIN run AS b ON b.id = d.blocker_id
                   WHERE d.run_id = r.id
                     AND NOT (b.state = ? AND b.exit IS ?)
               )
               AND NOT EXISTS (
                   SELECT 1
                   FROM session AS s
                   JOIN session AS o
                     ON o.subscription_profile_id = s.subscription_profile_id
                    AND o.runtime = s.runtime
                   JOIN run AS a ON a.session_id = o.id
                   WHERE s.id = r.session_id
                     AND s.runtime IN (SELECT value FROM json_each(?))
                     AND a.state = ?
               )
               AND (? IS NULL OR r.enqueued_at < ?)
             ORDER BY r.enqueued_at, r.id",
        )
        .bind(RunState::Queued.as_str())
        .bind(RunState::Ended.as_str())
        .bind(Exit::Succeeded.status())
        .bind(serde_json::to_string(serialized)?)
        .bind(RunState::Active.as_str())
        .bind(enqueued_before.map(due))
        .bind(enqueued_before.map(due))
        .fetch_all(&mut *self.connection)
        .await
        .context("reading claimable runs")?;

        let mut runs = Vec::with_capacity(ids.len());
        for id in ids {
            runs.push(self.run(id.get::<String, _>("id").parse()?).await?);
        }
        Ok(runs)
    }

    pub async fn claim_run(&mut self, run: &Run, lease_until: Timestamp) -> Result<Option<Run>> {
        let claimed = sqlx::query(
            "UPDATE run
             SET state = ?, waiting_for = NULL, claimed_at = ?, lease_expires_at = ?
             WHERE id = ? AND state = ?
             RETURNING id",
        )
        .bind(RunState::Active.as_str())
        .bind(Timestamp::now().to_string())
        .bind(due(lease_until))
        .bind(run.id.to_string())
        .bind(RunState::Queued.as_str())
        .fetch_optional(&mut *self.connection)
        .await
        .with_context(|| format!("claiming the queued run {}", run.id))?;

        match claimed {
            Some(_) => Ok(Some(self.run(run.id).await?)),
            None => Ok(None),
        }
    }

    pub async fn wait_for_instance(&mut self, run: &Run, because: &str) -> Result<()> {
        sqlx::query("UPDATE run SET waiting_for = ? WHERE id = ? AND state = ?")
            .bind(because)
            .bind(run.id.to_string())
            .bind(RunState::Queued.as_str())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording why the run {} is waiting", run.id))?;
        Ok(())
    }

    pub async fn run(&mut self, id: RunId) -> Result<Run> {
        let row = sqlx::query(runs_where!("id = ?"))
            .bind(id.to_string())
            .fetch_optional(&mut *self.connection)
            .await?
            .with_context(|| format!("no run {id}"))?;

        run(&row)
    }

    pub async fn runs(&mut self, session: &Session) -> Result<Vec<Run>> {
        sqlx::query(runs_where!("session_id = ? ORDER BY enqueued_at, id"))
            .bind(session.id.to_string())
            .fetch_all(&mut *self.connection)
            .await?
            .iter()
            .map(run)
            .collect()
    }

    pub async fn record_connected(&mut self, run: &Run, version: &str) -> Result<()> {
        sqlx::query("UPDATE run SET connected_at = ?, supervisor_version = ? WHERE id = ?")
            .bind(Timestamp::now().to_string())
            .bind(version)
            .bind(run.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording the supervisor of run {} connected", run.id))?;

        Ok(())
    }

    /// The figures are cumulative, so the last report of them is the one that stands.
    pub async fn record_usage(&mut self, run: &Run, usage: &Usage) -> Result<()> {
        sqlx::query(
            "UPDATE run
             SET context_used = ?, context_size = ?, cost_amount = ?, cost_currency = ?
             WHERE id = ?",
        )
        .bind(usage.context_used as i64)
        .bind(usage.context_size as i64)
        .bind(usage.cost.as_ref().map(|cost| cost.amount))
        .bind(usage.cost.as_ref().map(|cost| cost.currency.clone()))
        .bind(run.id.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("recording what run {} used", run.id))?;

        Ok(())
    }

    pub async fn record_worked_model(&mut self, run: &Run, model: &str) -> Result<()> {
        sqlx::query("UPDATE run SET worked_model = ? WHERE id = ?")
            .bind(model)
            .bind(run.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording the model run {} is on", run.id))?;

        Ok(())
    }

    pub async fn instance(&mut self, session: SessionId) -> Result<Option<String>> {
        let row = sqlx::query("SELECT instance FROM session WHERE id = ?")
            .bind(session.to_string())
            .fetch_one(&mut *self.connection)
            .await
            .with_context(|| format!("reading the instance of the session {session}"))?;

        Ok(row.get("instance"))
    }

    /// `None` forgets an Instance that is gone, so the Session's next Run provisions another.
    pub async fn record_instance(
        &mut self,
        session: SessionId,
        instance: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE session SET instance = ?, observed = NULL WHERE id = ?")
            .bind(instance)
            .bind(session.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording the instance of the session {session}"))?;

        Ok(())
    }

    pub async fn record_observed(
        &mut self,
        session: SessionId,
        observed: &[Observed],
    ) -> Result<()> {
        sqlx::query("UPDATE session SET observed = ? WHERE id = ? AND instance IS NOT NULL")
            .bind(serde_json::to_string(observed)?)
            .bind(session.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording what the session {session}'s checkout holds"))?;

        Ok(())
    }

    pub async fn kept_instance(&mut self, session: SessionId) -> Result<Option<Kept>> {
        sqlx::query(
            "SELECT id, instance, observed FROM session WHERE id = ? AND instance IS NOT NULL",
        )
        .bind(session.to_string())
        .fetch_optional(&mut *self.connection)
        .await
        .with_context(|| format!("reading the instance of the session {session}"))?
        .map(|row| kept(&row))
        .transpose()
    }

    pub async fn kept_instances(&mut self, organization: &Organization) -> Result<Vec<Kept>> {
        sqlx::query(
            "SELECT id, instance, observed
             FROM session
             WHERE organization_id = ? AND instance IS NOT NULL
             ORDER BY last_active_at, id",
        )
        .bind(organization.id.to_string())
        .fetch_all(&mut *self.connection)
        .await
        .context("reading the instances sessions keep")?
        .iter()
        .map(kept)
        .collect()
    }

    /// The Session lets go of its Instance at once, so no Run is handed one about to be destroyed.
    pub async fn archive_instance(&mut self, session: &Session, instance: &str) -> Result<()> {
        self.record_instance(session.id, None).await?;
        sqlx::query(
            "INSERT INTO instance_archive (instance, organization_id, session_id, queued_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(instance)
        .bind(session.organization.id.to_string())
        .bind(session.id.to_string())
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("queueing the instance {instance} to be archived"))?;

        Ok(())
    }

    pub async fn instances_to_archive(&mut self) -> Result<Vec<String>> {
        sqlx::query_scalar("SELECT instance FROM instance_archive ORDER BY queued_at, instance")
            .fetch_all(&mut *self.connection)
            .await
            .context("reading the instances waiting to be archived")
    }

    pub async fn live_instance_count(&mut self, organization: &Organization) -> Result<usize> {
        let count: i64 = sqlx::query_scalar(
            "SELECT
                 (SELECT COUNT(*) FROM session WHERE organization_id = ? AND instance IS NOT NULL)
               + (SELECT COUNT(*) FROM instance_archive WHERE organization_id = ?)
               + (SELECT COUNT(*)
                  FROM run
                  JOIN session ON session.id = run.session_id
                  WHERE run.organization_id = ?
                    AND run.state = ?
                    AND session.instance IS NULL)",
        )
        .bind(organization.id.to_string())
        .bind(organization.id.to_string())
        .bind(organization.id.to_string())
        .bind(RunState::Active.as_str())
        .fetch_one(&mut *self.connection)
        .await
        .context("counting an organization's live instances")?;
        Ok(count as usize)
    }

    pub async fn instance_being_archived(
        &mut self,
        organization: &Organization,
    ) -> Result<Option<String>> {
        sqlx::query_scalar(
            "SELECT instance FROM instance_archive
             WHERE organization_id = ?
             ORDER BY queued_at, instance
             LIMIT 1",
        )
        .bind(organization.id.to_string())
        .fetch_optional(&mut *self.connection)
        .await
        .context("reading an organization's instance being archived")
    }

    pub async fn instance_archived(&mut self, instance: &str) -> Result<()> {
        sqlx::query("DELETE FROM instance_archive WHERE instance = ?")
            .bind(instance)
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording the instance {instance} archived"))?;

        Ok(())
    }

    pub async fn record_run_instance(&mut self, run: &Run, instance: &str) -> Result<()> {
        sqlx::query("UPDATE run SET instance = ? WHERE id = ?")
            .bind(instance)
            .bind(run.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording the instance run {} executes on", run.id))?;

        Ok(())
    }

    pub async fn record_supervisor(&mut self, run: &Run, supervisor: &str) -> Result<()> {
        sqlx::query(
            "UPDATE run
             SET supervisor = ?, supervisor_state = 'present'
             WHERE id = ?",
        )
        .bind(supervisor)
        .bind(run.id.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("recording the supervisor of run {}", run.id))?;

        Ok(())
    }

    pub async fn record_supervisor_gone(&mut self, run: &Run) -> Result<()> {
        sqlx::query("UPDATE run SET supervisor_state = 'gone' WHERE id = ?")
            .bind(run.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording that run {}'s supervisor is gone", run.id))?;

        Ok(())
    }

    pub async fn supervisor_is_gone(&mut self, run: &Run) -> Result<bool> {
        let row = sqlx::query(
            "SELECT supervisor_state != 'present' AS is_gone
             FROM run
             WHERE id = ?",
        )
        .bind(run.id.to_string())
        .fetch_one(&mut *self.connection)
        .await?;

        Ok(row.get("is_gone"))
    }

    pub async fn supervisors_to_stop(&mut self) -> Result<Vec<(Run, String)>> {
        let rows = sqlx::query(
            "SELECT id, supervisor
             FROM run
             WHERE state = ? AND supervisor_state = 'present'
             ORDER BY ended_at, id",
        )
        .bind(RunState::Ended.as_str())
        .fetch_all(&mut *self.connection)
        .await?;

        let mut supervisors = Vec::with_capacity(rows.len());
        for row in rows {
            let run = self.run(row.get::<String, _>("id").parse()?).await?;
            supervisors.push((run, row.get("supervisor")));
        }

        Ok(supervisors)
    }

    /// Only a Run that is active holds one, so a heartbeat arriving after its Run ended puts
    /// no lease back.
    pub async fn hold_lease(&mut self, run: &Run, until: Timestamp) -> Result<()> {
        sqlx::query("UPDATE run SET lease_expires_at = ? WHERE id = ? AND state = ?")
            .bind(due(until))
            .bind(run.id.to_string())
            .bind(RunState::Active.as_str())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("holding the lease of run {} until {until}", run.id))?;

        Ok(())
    }

    pub async fn expired_leases(&mut self, at: Timestamp) -> Result<Vec<Run>> {
        sqlx::query(runs_where!(
            "lease_expires_at <= ? ORDER BY lease_expires_at"
        ))
        .bind(due(at))
        .fetch_all(&mut *self.connection)
        .await
        .context("sweeping expired leases")?
        .iter()
        .map(run)
        .collect()
    }

    /// `false` when the Run had already started, so a supervisor that reconnects and says
    /// so again adds no second Transcript entry.
    pub async fn record_started(&mut self, run: &Run) -> Result<bool> {
        let started =
            sqlx::query("UPDATE run SET started_at = ? WHERE id = ? AND started_at IS NULL")
                .bind(Timestamp::now().to_string())
                .bind(run.id.to_string())
                .execute(&mut *self.connection)
                .await
                .with_context(|| format!("recording the run {} started", run.id))?;

        Ok(started.rows_affected() > 0)
    }

    /// Read and write under the same write lock every `Tx` takes up front, so the check and
    /// whatever the report changes commit together or not at all (ADR-0004).
    pub async fn take_report(&mut self, run: &Run, seq: i64) -> Result<Taken> {
        let taken: i64 = sqlx::query("SELECT reports_taken FROM run WHERE id = ?")
            .bind(run.id.to_string())
            .fetch_one(&mut *self.connection)
            .await
            .with_context(|| format!("reading what run {} has reported", run.id))?
            .get("reports_taken");

        if seq != taken + 1 {
            return Ok(match (1..=taken).contains(&seq) {
                true => Taken::Again,
                false => Taken::Skipped,
            });
        }

        sqlx::query("UPDATE run SET reports_taken = ? WHERE id = ?")
            .bind(seq)
            .bind(run.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("taking the report {seq} of run {}", run.id))?;

        Ok(Taken::Next)
    }

    /// `false` when the Run had already ended: whoever ends it first decides its exit status.
    pub async fn end_run(&mut self, run: &Run, exit: &Exit) -> Result<bool> {
        let ended = sqlx::query(
            "UPDATE run
             SET state = ?, waiting_for = NULL, ended_at = ?, exit = ?, exit_because = ?, lease_expires_at = NULL
             WHERE id = ? AND state != ?",
        )
        .bind(RunState::Ended.as_str())
        .bind(Timestamp::now().to_string())
        .bind(exit.status())
        .bind(exit.because())
        .bind(run.id.to_string())
        .bind(RunState::Ended.as_str())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("ending the run {}", run.id))?;

        if ended.rows_affected() == 0 {
            return Ok(false);
        }
        self.record_active(run.session, Timestamp::now()).await?;

        Ok(true)
    }

    pub async fn issue_credential(
        &mut self,
        run: &Run,
        digest: &str,
        expires_at: Timestamp,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO run_credential (token_hash, run_id, organization_id, issued_at, expires_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(digest)
        .bind(run.id.to_string())
        .bind(run.organization.to_string())
        .bind(Timestamp::now().to_string())
        .bind(expires_at.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("issuing a credential for the run {}", run.id))?;

        Ok(())
    }

    pub async fn invalidate_credentials(&mut self, run: &Run) -> Result<()> {
        sqlx::query(
            "UPDATE run_credential
             SET invalidated_at = ?
             WHERE run_id = ? AND invalidated_at IS NULL",
        )
        .bind(Timestamp::now().to_string())
        .bind(run.id.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("invalidating the credentials of run {}", run.id))?;

        Ok(())
    }

    pub async fn credential(&mut self, digest: &str) -> Result<Option<Credential>> {
        let found = sqlx::query(
            "SELECT run_id, organization_id, expires_at, invalidated_at
             FROM run_credential
             WHERE token_hash = ?",
        )
        .bind(digest)
        .fetch_optional(&mut *self.connection)
        .await?;

        found
            .map(|row| {
                Ok(Credential {
                    run: row.get::<String, _>("run_id").parse()?,
                    organization: row.get::<String, _>("organization_id").parse()?,
                    expires_at: row.get::<String, _>("expires_at").parse()?,
                    invalidated_at: row
                        .get::<Option<String>, _>("invalidated_at")
                        .map(|at| at.parse())
                        .transpose()?,
                })
            })
            .transpose()
    }

    pub async fn send_instruction(
        &mut self,
        run: &Run,
        instruction: Instruction,
    ) -> Result<SentInstruction> {
        let sent = sqlx::query(
            "INSERT INTO link_instruction (run_id, organization_id, seq, body, sent_at)
             VALUES (
                 ?,
                 ?,
                 (SELECT COALESCE(MAX(seq), 0) + 1 FROM link_instruction WHERE run_id = ?),
                 ?,
                 ?
             )
             RETURNING seq",
        )
        .bind(run.id.to_string())
        .bind(run.organization.to_string())
        .bind(run.id.to_string())
        .bind(serde_json::to_string(&instruction)?)
        .bind(Timestamp::now().to_string())
        .fetch_one(&mut *self.connection)
        .await
        .with_context(|| format!("sending an instruction to the run {}", run.id))?;

        Ok(SentInstruction {
            seq: sent.get("seq"),
            instruction,
        })
    }

    pub async fn instructions_after(
        &mut self,
        run: RunId,
        cursor: i64,
    ) -> Result<Vec<SentInstruction>> {
        sqlx::query(
            "SELECT seq, body
             FROM link_instruction
             WHERE run_id = ? AND seq > ?
             ORDER BY seq",
        )
        .bind(run.to_string())
        .bind(cursor)
        .fetch_all(&mut *self.connection)
        .await?
        .iter()
        .map(|row| {
            Ok(SentInstruction {
                seq: row.get("seq"),
                instruction: serde_json::from_str(row.get("body"))?,
            })
        })
        .collect()
    }

    /// The Turn is anchored to the last Transcript entry before its prompt, so what the Agent
    /// says during it is what follows that, never anything said before.
    pub async fn prompt_turn(&mut self, run: &Run) -> Result<i64> {
        let prompted = sqlx::query(
            "INSERT INTO turn (run_id, organization_id, seq, prompted_at, from_seq)
             VALUES (
                 ?,
                 ?,
                 (SELECT COALESCE(MAX(seq), 0) + 1 FROM turn WHERE run_id = ?),
                 ?,
                 (SELECT COALESCE(MAX(seq), 0) FROM transcript_entry
                   WHERE session_id = (SELECT session_id FROM run WHERE id = ?))
             )
             RETURNING seq",
        )
        .bind(run.id.to_string())
        .bind(run.organization.to_string())
        .bind(run.id.to_string())
        .bind(Timestamp::now().to_string())
        .bind(run.id.to_string())
        .fetch_one(&mut *self.connection)
        .await
        .with_context(|| format!("prompting a turn of the run {}", run.id))?;

        Ok(prompted.get("seq"))
    }

    /// The seq of the Turn waiting on an answer and the Transcript position its prompt followed,
    /// or `None` when no Turn was waiting, so an answer replayed after a reconnect closes
    /// nothing twice.
    pub async fn answer_turn(&mut self, run: &Run) -> Result<Option<(i64, i64)>> {
        let answered = sqlx::query(
            "UPDATE turn SET answered_at = ?
             WHERE run_id = ? AND answered_at IS NULL
             RETURNING seq, from_seq",
        )
        .bind(Timestamp::now().to_string())
        .bind(run.id.to_string())
        .fetch_optional(&mut *self.connection)
        .await
        .with_context(|| format!("answering the turn of the run {}", run.id))?;

        Ok(answered.map(|row| (row.get("seq"), row.get("from_seq"))))
    }

    pub async fn turns(&mut self, run: RunId) -> Result<Vec<Turn>> {
        sqlx::query(
            "SELECT seq, prompted_at, answered_at
             FROM turn
             WHERE run_id = ?
             ORDER BY seq",
        )
        .bind(run.to_string())
        .fetch_all(&mut *self.connection)
        .await
        .with_context(|| format!("reading the turns of the run {run}"))?
        .iter()
        .map(|row| {
            Ok(Turn {
                seq: row.get("seq"),
                prompted_at: row.get::<String, _>("prompted_at").parse()?,
                answered_at: timestamp(row, "answered_at")?,
            })
        })
        .collect()
    }

    pub async fn occupying_slots(&mut self) -> Result<usize> {
        let row = sqlx::query(concat!(
            "SELECT COUNT(*) AS occupying
             FROM run AS r
             WHERE r.state = ? AND NOT (",
            between_turns!(),
            ")"
        ))
        .bind(RunState::Active.as_str())
        .fetch_one(&mut *self.connection)
        .await
        .context("counting the runs occupying an active-work slot")?;

        Ok(usize::try_from(row.get::<i64, _>("occupying"))?)
    }

    pub async fn oldest_held_input(&mut self) -> Result<Option<(Run, Timestamp)>> {
        let row = sqlx::query(concat!(
            "SELECT r.id, MIN(p.received_at) AS since
             FROM run AS r
             JOIN pending_message AS p ON p.session_id = r.session_id
             WHERE r.state = ? AND ",
            between_turns!(),
            " GROUP BY r.id
             ORDER BY since, r.id
             LIMIT 1"
        ))
        .bind(RunState::Active.as_str())
        .fetch_optional(&mut *self.connection)
        .await
        .context("reading which run between turns has input held longest")?;

        let Some(row) = row else {
            return Ok(None);
        };
        let since = row.get::<String, _>("since").parse()?;

        Ok(Some((
            self.run(row.get::<String, _>("id").parse()?).await?,
            since,
        )))
    }

    pub async fn is_waiting(&mut self, run: &Run) -> Result<bool> {
        let row = sqlx::query(concat!(
            "SELECT EXISTS (SELECT 1 FROM run AS r WHERE r.id = ? AND r.state = ? AND ",
            between_turns!(),
            ") AS waiting"
        ))
        .bind(run.id.to_string())
        .bind(RunState::Active.as_str())
        .fetch_one(&mut *self.connection)
        .await
        .with_context(|| format!("reading whether the run {} is between turns", run.id))?;

        Ok(row.get("waiting"))
    }
}

pub(crate) async fn read(connection: &mut SqliteConnection, id: SessionId) -> Result<Session> {
    find(connection, id)
        .await?
        .with_context(|| format!("no session {id}"))
}

async fn find(connection: &mut SqliteConnection, id: SessionId) -> Result<Option<Session>> {
    let Some(row) = sqlx::query(
        "SELECT name, organization_id, workspace_id, agent_id, runtime, model, subscription_profile_id,
                base, branch, correlation, state, opened_at, last_active_at, sealed_at,
                continues, event_record_id
         FROM session
         WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(&mut *connection)
    .await?
    else {
        return Ok(None);
    };

    let organization =
        organization::with_id(connection, row.get::<String, _>("organization_id").parse()?).await?;
    let workspace = workspace::with_id(
        connection,
        &organization,
        row.get::<String, _>("workspace_id").parse()?,
    )
    .await?;
    // What the Agent was declared as when the session opened, not what it has been redeclared as.
    let agent = Agent {
        runtime: row.get("runtime"),
        model: row.get("model"),
        ..agent::with_id(
            connection,
            &organization,
            row.get::<String, _>("agent_id").parse()?,
        )
        .await?
    };
    let profile = match row.get::<Option<String>, _>("subscription_profile_id") {
        Some(id) => Some(profile::with_id(connection, id.parse()?).await?),
        None => None,
    };
    let repositories =
        sqlx::query("SELECT url FROM session_repository WHERE session_id = ? ORDER BY position")
            .bind(id.to_string())
            .fetch_all(&mut *connection)
            .await?
            .iter()
            .map(|row| row.get("url"))
            .collect();

    Ok(Some(Session {
        id,
        name: row.get("name"),
        organization,
        workspace,
        agent,
        profile,
        checkout: Checkout {
            repositories,
            base: row.get("base"),
            branch: row.get("branch"),
        },
        correlation: row.get("correlation"),
        state: row.get::<String, _>("state").parse()?,
        opened_at: row.get::<String, _>("opened_at").parse()?,
        last_active_at: row.get::<String, _>("last_active_at").parse()?,
        sealed_at: timestamp(&row, "sealed_at")?,
        continues: row
            .get::<Option<String>, _>("continues")
            .map(|sealed| sealed.parse())
            .transpose()?,
        started_by: row
            .get::<Option<String>, _>("event_record_id")
            .map(|event| event.parse())
            .transpose()?,
    }))
}

fn candidate(row: &SqliteRow) -> Candidate {
    Candidate {
        id: row.get("id"),
        name: row.get("name"),
    }
}

fn run(row: &SqliteRow) -> Result<Run> {
    let exit: Option<String> = row.get("exit");
    let connected_at: Option<String> = row.get("connected_at");

    Ok(Run {
        id: row.get::<String, _>("id").parse()?,
        name: row.get("name"),
        organization: row.get::<String, _>("organization_id").parse()?,
        session: row.get::<String, _>("session_id").parse()?,
        state: row.get::<String, _>("state").parse()?,
        waiting_for: row.get("waiting_for"),
        exit: exit
            .map(|status| Exit::read(&status, row.get("exit_because")))
            .transpose()?,
        instance: row.get("instance"),
        supervisor: row.get("supervisor"),
        model: row.get("model"),
        worked_model: row.get("worked_model"),
        enqueued_at: row.get::<String, _>("enqueued_at").parse()?,
        started_at: timestamp(row, "started_at")?,
        ended_at: timestamp(row, "ended_at")?,
        lease_expires_at: timestamp(row, "lease_expires_at")?,
        connected: match connected_at {
            Some(at) => Some(Connected {
                at: at.parse()?,
                version: row.get("supervisor_version"),
            }),
            None => None,
        },
        usage: usage(row),
    })
}

fn generated_name() -> String {
    const ADJECTIVES: &[&str] = &[
        "agile", "amber", "brisk", "bright", "calm", "clever", "coral", "crisp", "daring", "eager",
        "ember", "fable", "gentle", "golden", "grand", "happy", "hidden", "jolly", "keen", "kind",
        "lively", "lucky", "merry", "mighty", "nimble", "noble", "plucky", "proud", "quick",
        "quiet", "rapid", "silver",
    ];
    const NOUNS: &[&str] = &[
        "badger", "beacon", "cedar", "comet", "falcon", "fern", "fox", "harbor", "heron",
        "juniper", "kite", "lantern", "maple", "meadow", "otter", "owl", "pebble", "pioneer",
        "raven", "river", "robin", "sailor", "sparrow", "summit", "thistle", "valley", "willow",
        "wren", "yarrow", "zephyr", "acorn", "brook",
    ];

    let mut bytes = [0; 10];
    getrandom::fill(&mut bytes).expect("the operating system should have entropy to spare");
    let suffix: String = bytes[2..]
        .iter()
        .map(|byte| char::from(b'a' + byte % 26))
        .collect();
    format!(
        "{}-{}-{suffix}",
        ADJECTIVES[usize::from(bytes[0]) % ADJECTIVES.len()],
        NOUNS[usize::from(bytes[1]) % NOUNS.len()]
    )
}

fn kept(row: &SqliteRow) -> Result<Kept> {
    Ok(Kept {
        session: row.get::<String, _>("id").parse()?,
        instance: row.get("instance"),
        observed: row
            .get::<Option<String>, _>("observed")
            .map(|observed| serde_json::from_str(&observed))
            .transpose()?,
    })
}

fn usage(row: &SqliteRow) -> Option<Usage> {
    let context_used: Option<i64> = row.get("context_used");
    let currency: Option<String> = row.get("cost_currency");

    Some(Usage {
        context_used: context_used? as u64,
        context_size: row.get::<i64, _>("context_size") as u64,
        cost: match (row.get::<Option<f64>, _>("cost_amount"), currency) {
            (Some(amount), Some(currency)) => Some(Cost { amount, currency }),
            _ => None,
        },
    })
}
