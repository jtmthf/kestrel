use anyhow::{Context as _, Result};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use crate::domain::{
    Agent, Connected, Cost, Event, Exit, Organization, Run, RunId, RunState, Session, SessionId,
    SessionState, Usage, Workspace,
};
use crate::link::credential::Credential;
use crate::link::{Instruction, SentInstruction};
use crate::store::{agent, due, organization, timestamp, workspace};

macro_rules! runs_where {
    ($tail:literal) => {
        concat!(
            "SELECT id, organization_id, session_id, state, exit, exit_because, environment,
                    enqueued_at, started_at, ended_at, lease_expires_at, connected_at,
                    supervisor_version, model, context_used, context_size, cost_amount,
                    cost_currency
             FROM run
             WHERE ",
            $tail
        )
    };
}

/// What became of a report the link was handed: the next in the Environment's sequence, one
/// taken already — where a replay after an answer that never arrived lands — or one that
/// skips a report the Run has yet to make, which would leave a gap nothing fills.
pub enum Taken {
    Next,
    Again,
    Skipped,
}

pub struct PendingMessage {
    pub participant: String,
    pub body: String,
}

pub struct Sessions<'a> {
    connection: &'a mut SqliteConnection,
}

impl<'a> Sessions<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection) -> Self {
        Self { connection }
    }

    pub async fn open(
        &mut self,
        organization: &Organization,
        workspace: &Workspace,
        agent: &Agent,
        continues: Option<&Session>,
        started_by: Option<&Event>,
    ) -> Result<Session> {
        let opened_at = Timestamp::now();
        let session = Session {
            id: SessionId::generate(),
            organization: organization.clone(),
            workspace: workspace.clone(),
            agent: agent.clone(),
            state: SessionState::Open,
            opened_at,
            last_active_at: opened_at,
            sealed_at: None,
            continues: continues.map(|sealed| sealed.id),
            started_by: started_by.map(|event| event.id),
        };

        sqlx::query(
            "INSERT INTO session
                 (id, organization_id, workspace_id, agent_id, state, opened_at, last_active_at,
                  continues, event_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(session.id.to_string())
        .bind(organization.id.to_string())
        .bind(workspace.id.to_string())
        .bind(agent.id.to_string())
        .bind(session.state.as_str())
        .bind(session.opened_at.to_string())
        .bind(due(session.last_active_at))
        .bind(session.continues.map(|sealed| sealed.to_string()))
        .bind(session.started_by.map(|event| event.to_string()))
        .execute(&mut *self.connection)
        .await
        .context("opening a session")?;

        Ok(session)
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
    /// dispatched: two Runs queued in one Session would otherwise both be handed out.
    pub async fn run_holding_the_slot(&mut self, session: &Session) -> Result<Option<RunId>> {
        let holding = sqlx::query(
            "SELECT id
             FROM run
             WHERE session_id = ?
               AND (state != ? OR environment_state = 'present')
             ORDER BY enqueued_at, id
             LIMIT 1",
        )
        .bind(session.id.to_string())
        .bind(RunState::Ended.as_str())
        .fetch_optional(&mut *self.connection)
        .await
        .with_context(|| format!("reading what run the session {} has", session.id))?;

        holding
            .map(|row| Ok(row.get::<String, _>("id").parse()?))
            .transpose()
    }

    pub async fn enqueue_run(&mut self, session: &Session) -> Result<Run> {
        let run = Run {
            id: RunId::generate(),
            organization: session.organization.id,
            session: session.id,
            state: RunState::Queued,
            exit: None,
            environment: None,
            model: None,
            enqueued_at: Timestamp::now(),
            started_at: None,
            ended_at: None,
            lease_expires_at: None,
            connected: None,
            usage: None,
        };

        sqlx::query(
            "INSERT INTO run (id, organization_id, session_id, state, enqueued_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(run.id.to_string())
        .bind(run.organization.to_string())
        .bind(run.session.to_string())
        .bind(run.state.as_str())
        .bind(run.enqueued_at.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("enqueueing a run in the session {}", session.id))?;

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
        .bind(Timestamp::now().to_string())
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

    /// One statement, so two claimants cannot both take the same Run: the Run this returns
    /// was queued when the statement began, and is active and holding its lease by the time
    /// anyone else looks.
    pub async fn claim_run(&mut self, lease_until: Timestamp) -> Result<Option<Run>> {
        let claimed = sqlx::query(
            "UPDATE run
             SET state = ?, claimed_at = ?, lease_expires_at = ?
             WHERE id = (
                 SELECT id FROM run WHERE state = ? ORDER BY enqueued_at, id LIMIT 1
             )
             RETURNING id",
        )
        .bind(RunState::Active.as_str())
        .bind(Timestamp::now().to_string())
        .bind(due(lease_until))
        .bind(RunState::Queued.as_str())
        .fetch_optional(&mut *self.connection)
        .await
        .context("claiming a queued run")?;

        match claimed {
            Some(claimed) => Ok(Some(
                self.run(claimed.get::<String, _>("id").parse()?).await?,
            )),
            None => Ok(None),
        }
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
            .with_context(|| format!("recording the environment of run {} connected", run.id))?;

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

    pub async fn record_model(&mut self, run: &Run, model: &str) -> Result<()> {
        sqlx::query("UPDATE run SET model = ? WHERE id = ?")
            .bind(model)
            .bind(run.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording the model run {} is on", run.id))?;

        Ok(())
    }

    pub async fn record_environment(&mut self, run: &Run, environment: &str) -> Result<()> {
        sqlx::query(
            "UPDATE run
             SET environment = ?, environment_instance = ?, environment_state = 'present'
             WHERE id = ?",
        )
        .bind(environment)
        .bind(environment)
        .bind(run.id.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("recording the environment run {} executes in", run.id))?;

        Ok(())
    }

    pub async fn record_environment_present(&mut self, run: &Run, environment: &str) -> Result<()> {
        sqlx::query(
            "UPDATE run
             SET environment_state = 'present', environment_instance = ?
             WHERE id = ?",
        )
        .bind(environment)
        .bind(run.id.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("recording that run {} has an environment", run.id))?;

        Ok(())
    }

    pub async fn record_environment_gone(&mut self, run: &Run) -> Result<()> {
        sqlx::query("UPDATE run SET environment_state = 'gone' WHERE id = ?")
            .bind(run.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("recording that run {}'s environment is gone", run.id))?;

        Ok(())
    }

    pub async fn environment_is_gone(&mut self, run: &Run) -> Result<bool> {
        let row = sqlx::query(
            "SELECT environment_state != 'present' AS is_gone
             FROM run
             WHERE id = ?",
        )
        .bind(run.id.to_string())
        .fetch_one(&mut *self.connection)
        .await?;

        Ok(row.get("is_gone"))
    }

    pub async fn environments_to_reap(&mut self) -> Result<Vec<(Run, String)>> {
        let rows = sqlx::query(
            "SELECT id, environment_instance
             FROM run
             WHERE state = ? AND environment_state = 'present'
             ORDER BY ended_at, id",
        )
        .bind(RunState::Ended.as_str())
        .fetch_all(&mut *self.connection)
        .await?;

        let mut environments = Vec::with_capacity(rows.len());
        for row in rows {
            let run = self.run(row.get::<String, _>("id").parse()?).await?;
            environments.push((run, row.get("environment_instance")));
        }

        Ok(environments)
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

    /// `false` when the Run had already started, so an Environment that reconnects and says
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
             SET state = ?, ended_at = ?, exit = ?, exit_because = ?, lease_expires_at = NULL
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
}

pub(crate) async fn read(connection: &mut SqliteConnection, id: SessionId) -> Result<Session> {
    let row = sqlx::query(
        "SELECT organization_id, workspace_id, agent_id, state, opened_at, last_active_at,
                sealed_at, continues, event_id
         FROM session
         WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(&mut *connection)
    .await?
    .with_context(|| format!("no session {id}"))?;

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

    Ok(Session {
        id,
        organization,
        workspace,
        agent,
        state: row.get::<String, _>("state").parse()?,
        opened_at: row.get::<String, _>("opened_at").parse()?,
        last_active_at: row.get::<String, _>("last_active_at").parse()?,
        sealed_at: timestamp(&row, "sealed_at")?,
        continues: row
            .get::<Option<String>, _>("continues")
            .map(|sealed| sealed.parse())
            .transpose()?,
        started_by: row
            .get::<Option<String>, _>("event_id")
            .map(|event| event.parse())
            .transpose()?,
    })
}

fn run(row: &SqliteRow) -> Result<Run> {
    let exit: Option<String> = row.get("exit");
    let connected_at: Option<String> = row.get("connected_at");

    Ok(Run {
        id: row.get::<String, _>("id").parse()?,
        organization: row.get::<String, _>("organization_id").parse()?,
        session: row.get::<String, _>("session_id").parse()?,
        state: row.get::<String, _>("state").parse()?,
        exit: exit
            .map(|status| Exit::read(&status, row.get("exit_because")))
            .transpose()?,
        environment: row.get("environment"),
        model: row.get("model"),
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
