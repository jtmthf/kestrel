use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use jiff::{SignedDuration, Timestamp};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteRow};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::domain::{
    Agent, AgentId, Connected, Cost, Direction, Event, EventId, Exit, Integration, IntegrationId,
    IntegrationKind, Occurrence, Organization, OrganizationId, Outcome, Run, RunId, RunState,
    Session, SessionId, SessionState, Trigger, TriggerId, TriggerState, Usage, Workspace,
    WorkspaceId,
};
use crate::integration::credential::Token;
use crate::keyring::Keyring;
use crate::link::credential::Credential;
use crate::link::{Instruction, SentInstruction};
use crate::log::Log;
use crate::provider::Held;

const DATABASE: &str = "kestrel.db";

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

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    keyring: Arc<Keyring>,
}

impl Store {
    pub async fn open(data_dir: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(data_dir)
            .await
            .with_context(|| format!("creating kestrel's data directory {}", data_dir.display()))?;

        let options = SqliteConnectOptions::new()
            .filename(data_dir.join(DATABASE))
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);

        let pool = SqlitePool::connect_with(options)
            .await
            .with_context(|| format!("opening kestrel's database in {}", data_dir.display()))?;

        sqlx::migrate!("src/store/migrations")
            .run(&pool)
            .await
            .context("migrating kestrel's database")?;

        Ok(Self {
            pool,
            keyring: Arc::new(Keyring::beside(data_dir)?),
        })
    }

    /// A `Tx` that is dropped rather than committed rolls back, which is how a read is scoped
    /// too. Every one of them takes the write lock up front: SQLite refuses a deferred
    /// transaction that reads and then writes while another has written, rather than making
    /// it wait its turn.
    pub async fn begin(&self) -> Result<Tx<'_>> {
        Ok(Tx {
            transaction: self.pool.begin_with("BEGIN IMMEDIATE").await?,
            keyring: &self.keyring,
        })
    }
}

pub struct Tx<'a> {
    transaction: Transaction<'a, Sqlite>,
    keyring: &'a Keyring,
}

impl Tx<'_> {
    pub fn log(&mut self) -> Log<'_> {
        Log::over(&mut self.transaction)
    }

    pub async fn commit(self) -> Result<()> {
        self.transaction.commit().await?;
        Ok(())
    }

    pub async fn declare_organization(&mut self, name: &str) -> Result<Organization> {
        let organization = Organization {
            id: OrganizationId::generate(),
            name: name.to_owned(),
        };

        sqlx::query("INSERT INTO organization (id, name, declared_at) VALUES (?, ?, ?)")
            .bind(organization.id.to_string())
            .bind(&organization.name)
            .bind(Timestamp::now().to_string())
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("declaring the organization {name}"))?;

        Ok(organization)
    }

    pub async fn organizations(&mut self) -> Result<Vec<Organization>> {
        sqlx::query("SELECT id, name FROM organization ORDER BY name")
            .fetch_all(&mut *self.transaction)
            .await?
            .iter()
            .map(organization)
            .collect()
    }

    pub async fn organization_named(&mut self, name: &str) -> Result<Organization> {
        let found = sqlx::query("SELECT id, name FROM organization WHERE name = ?")
            .bind(name)
            .fetch_optional(&mut *self.transaction)
            .await?
            .with_context(|| format!("no organization named {name}"))?;

        organization(&found)
    }

    pub async fn declare_workspace(
        &mut self,
        organization: &Organization,
        name: &str,
        repositories: &[String],
        branch: &str,
    ) -> Result<Workspace> {
        let workspace = Workspace {
            id: WorkspaceId::generate(),
            organization: organization.id,
            name: name.to_owned(),
            repositories: repositories.to_vec(),
            branch: branch.to_owned(),
        };

        sqlx::query(
            "INSERT INTO workspace (id, organization_id, name, branch, declared_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(workspace.id.to_string())
        .bind(workspace.organization.to_string())
        .bind(&workspace.name)
        .bind(&workspace.branch)
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("declaring the workspace {name}"))?;

        for (position, url) in workspace.repositories.iter().enumerate() {
            sqlx::query(
                "INSERT INTO workspace_repository (workspace_id, organization_id, position, url)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(workspace.id.to_string())
            .bind(workspace.organization.to_string())
            .bind(i64::try_from(position)?)
            .bind(url)
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("naming the repository {url} in the workspace {name}"))?;
        }

        Ok(workspace)
    }

    pub async fn workspace_named(
        &mut self,
        organization: &Organization,
        name: &str,
    ) -> Result<Workspace> {
        let found = sqlx::query("SELECT id FROM workspace WHERE organization_id = ? AND name = ?")
            .bind(organization.id.to_string())
            .bind(name)
            .fetch_optional(&mut *self.transaction)
            .await?
            .with_context(|| {
                format!(
                    "no workspace named {name} in the organization {}",
                    organization.name
                )
            })?;

        self.workspace_with_id(organization, found.get::<String, _>("id").parse()?)
            .await
    }

    pub async fn workspaces(&mut self, organization: &Organization) -> Result<Vec<Workspace>> {
        let rows = sqlx::query(
            "SELECT workspace.id, workspace.name, workspace.branch, workspace_repository.url
             FROM workspace
             LEFT JOIN workspace_repository ON workspace_repository.workspace_id = workspace.id
             WHERE workspace.organization_id = ?
             ORDER BY workspace.name, workspace_repository.position",
        )
        .bind(organization.id.to_string())
        .fetch_all(&mut *self.transaction)
        .await?;

        workspaces(&rows, organization)
    }

    pub async fn declare_agent(
        &mut self,
        organization: &Organization,
        name: &str,
        runtime: &str,
        model: Option<&str>,
    ) -> Result<Agent> {
        let agent = Agent {
            id: AgentId::generate(),
            organization: organization.id,
            name: name.to_owned(),
            runtime: runtime.to_owned(),
            model: model.map(str::to_owned),
        };

        sqlx::query(
            "INSERT INTO agent (id, organization_id, name, runtime, model, declared_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(agent.id.to_string())
        .bind(agent.organization.to_string())
        .bind(&agent.name)
        .bind(&agent.runtime)
        .bind(&agent.model)
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("declaring the agent {name}"))?;

        Ok(agent)
    }

    pub async fn agent_named(&mut self, organization: &Organization, name: &str) -> Result<Agent> {
        let found = sqlx::query(
            "SELECT id, organization_id, name, runtime, model
             FROM agent
             WHERE organization_id = ? AND name = ?",
        )
        .bind(organization.id.to_string())
        .bind(name)
        .fetch_optional(&mut *self.transaction)
        .await?
        .with_context(|| {
            format!(
                "no agent named {name} in the organization {}",
                organization.name
            )
        })?;

        agent(&found)
    }

    /// A Run in flight was provisioned with the model its Agent named when it was dispatched,
    /// and is not reached by this.
    pub async fn set_agent_model(&mut self, agent: &Agent, model: Option<&str>) -> Result<Agent> {
        sqlx::query("UPDATE agent SET model = ? WHERE id = ?")
            .bind(model)
            .bind(agent.id.to_string())
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("changing the model the agent {} works with", agent.name))?;

        Ok(Agent {
            model: model.map(str::to_owned),
            ..agent.clone()
        })
    }

    pub async fn open_session(
        &mut self,
        organization: &Organization,
        workspace: &Workspace,
        agent: &Agent,
        continues: Option<&Session>,
        started_by: Option<&Event>,
    ) -> Result<Session> {
        let session = Session {
            id: SessionId::generate(),
            organization: organization.clone(),
            workspace: workspace.clone(),
            agent: agent.clone(),
            state: SessionState::Open,
            opened_at: Timestamp::now(),
            sealed_at: None,
            continues: continues.map(|sealed| sealed.id),
            started_by: started_by.map(|event| event.id),
        };

        sqlx::query(
            "INSERT INTO session
                 (id, organization_id, workspace_id, agent_id, state, opened_at, continues,
                  event_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(session.id.to_string())
        .bind(organization.id.to_string())
        .bind(workspace.id.to_string())
        .bind(agent.id.to_string())
        .bind(session.state.as_str())
        .bind(session.opened_at.to_string())
        .bind(session.continues.map(|sealed| sealed.to_string()))
        .bind(session.started_by.map(|event| event.to_string()))
        .execute(&mut *self.transaction)
        .await
        .context("opening a session")?;

        Ok(session)
    }

    pub async fn seal_session(&mut self, session: &Session) -> Result<Timestamp> {
        let sealed_at = Timestamp::now();

        sqlx::query("UPDATE session SET state = ?, sealed_at = ? WHERE id = ?")
            .bind(SessionState::Sealed.as_str())
            .bind(sealed_at.to_string())
            .bind(session.id.to_string())
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("sealing the session {}", session.id))?;

        Ok(sealed_at)
    }

    /// The slot is taken from the moment work is enqueued rather than from the moment it is
    /// dispatched: two Runs queued in one Session would otherwise both be handed out.
    pub async fn run_holding_the_slot(&mut self, session: &Session) -> Result<Option<RunId>> {
        let holding = sqlx::query(
            "SELECT id
             FROM run
             WHERE session_id = ? AND state != ?
             ORDER BY enqueued_at, id
             LIMIT 1",
        )
        .bind(session.id.to_string())
        .bind(RunState::Ended.as_str())
        .fetch_optional(&mut *self.transaction)
        .await
        .with_context(|| format!("reading what run the session {} has", session.id))?;

        holding
            .map(|row| Ok(row.get::<String, _>("id").parse()?))
            .transpose()
    }

    pub async fn session(&mut self, id: SessionId) -> Result<Session> {
        let row = sqlx::query(
            "SELECT organization_id, workspace_id, agent_id, state, opened_at, sealed_at,
                    continues, event_id
             FROM session
             WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&mut *self.transaction)
        .await?
        .with_context(|| format!("no session {id}"))?;

        let organization = self
            .organization_with_id(row.get::<String, _>("organization_id").parse()?)
            .await?;
        let workspace = self
            .workspace_with_id(&organization, row.get::<String, _>("workspace_id").parse()?)
            .await?;
        let agent = self
            .agent_with_id(&organization, row.get::<String, _>("agent_id").parse()?)
            .await?;

        Ok(Session {
            id,
            organization,
            workspace,
            agent,
            state: row.get::<String, _>("state").parse()?,
            opened_at: row.get::<String, _>("opened_at").parse()?,
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

    pub async fn sessions(&mut self, organization: &Organization) -> Result<Vec<Session>> {
        let ids =
            sqlx::query("SELECT id FROM session WHERE organization_id = ? ORDER BY opened_at, id")
                .bind(organization.id.to_string())
                .fetch_all(&mut *self.transaction)
                .await
                .context("reading an organization's sessions")?
                .iter()
                .map(|row| Ok(row.get::<String, _>("id").parse()?))
                .collect::<Result<Vec<SessionId>>>()?;

        let mut sessions = Vec::with_capacity(ids.len());
        for id in ids {
            sessions.push(self.session(id).await?);
        }

        Ok(sessions)
    }

    /// Read on its own rather than with the Session: every path that reports a Run reads one,
    /// and none of them looks at what continues it.
    pub async fn continuations(&mut self, sealed: SessionId) -> Result<Vec<SessionId>> {
        sqlx::query("SELECT id FROM session WHERE continues = ? ORDER BY opened_at, id")
            .bind(sealed.to_string())
            .fetch_all(&mut *self.transaction)
            .await
            .with_context(|| format!("reading what continues the session {sealed}"))?
            .iter()
            .map(|row| Ok(row.get::<String, _>("id").parse()?))
            .collect()
    }

    async fn organization_with_id(&mut self, id: OrganizationId) -> Result<Organization> {
        let row = sqlx::query("SELECT id, name FROM organization WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(&mut *self.transaction)
            .await?;

        organization(&row)
    }

    async fn workspace_with_id(
        &mut self,
        organization: &Organization,
        id: WorkspaceId,
    ) -> Result<Workspace> {
        let rows = sqlx::query(
            "SELECT workspace.id, workspace.name, workspace.branch, workspace_repository.url
             FROM workspace
             LEFT JOIN workspace_repository ON workspace_repository.workspace_id = workspace.id
             WHERE workspace.organization_id = ? AND workspace.id = ?
             ORDER BY workspace_repository.position",
        )
        .bind(organization.id.to_string())
        .bind(id.to_string())
        .fetch_all(&mut *self.transaction)
        .await?;

        workspaces(&rows, organization)?
            .pop()
            .with_context(|| format!("no workspace {id}"))
    }

    async fn agent_with_id(&mut self, organization: &Organization, id: AgentId) -> Result<Agent> {
        let row = sqlx::query(
            "SELECT id, organization_id, name, runtime, model
             FROM agent
             WHERE organization_id = ? AND id = ?",
        )
        .bind(organization.id.to_string())
        .bind(id.to_string())
        .fetch_one(&mut *self.transaction)
        .await?;

        agent(&row)
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
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("enqueueing a run in the session {}", session.id))?;

        Ok(run)
    }

    pub async fn mark_turn_pending(&mut self, session: &Session) -> Result<()> {
        sqlx::query("UPDATE session SET turn_pending = TRUE WHERE id = ?")
            .bind(session.id.to_string())
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("recording a pending turn in the session {}", session.id))?;

        Ok(())
    }

    pub async fn take_pending_turn(&mut self, session: &Session) -> Result<bool> {
        let taken = sqlx::query(
            "UPDATE session SET turn_pending = FALSE WHERE id = ? AND turn_pending = TRUE",
        )
        .bind(session.id.to_string())
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("taking the pending turn in the session {}", session.id))?;

        Ok(taken.rows_affected() > 0)
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
        .fetch_optional(&mut *self.transaction)
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
            .fetch_optional(&mut *self.transaction)
            .await?
            .with_context(|| format!("no run {id}"))?;

        run(&row)
    }

    pub async fn runs(&mut self, session: &Session) -> Result<Vec<Run>> {
        sqlx::query(runs_where!("session_id = ? ORDER BY enqueued_at, id"))
            .bind(session.id.to_string())
            .fetch_all(&mut *self.transaction)
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
            .execute(&mut *self.transaction)
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
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("recording what run {} used", run.id))?;

        Ok(())
    }

    pub async fn record_model(&mut self, run: &Run, model: &str) -> Result<()> {
        sqlx::query("UPDATE run SET model = ? WHERE id = ?")
            .bind(model)
            .bind(run.id.to_string())
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("recording the model run {} is on", run.id))?;

        Ok(())
    }

    /// What one Agent Runtime advertised, kept per Organization because an installation of it
    /// offers what that organization's own configuration reaches.
    pub async fn record_models_advertised(
        &mut self,
        organization: OrganizationId,
        runtime: &str,
        models: &[String],
    ) -> Result<()> {
        for model in models {
            sqlx::query(
                "INSERT INTO runtime_model (organization_id, runtime, model, advertised_at)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT (organization_id, runtime, model)
                 DO UPDATE SET advertised_at = excluded.advertised_at",
            )
            .bind(organization.to_string())
            .bind(runtime)
            .bind(model)
            .bind(Timestamp::now().to_string())
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("recording that {runtime} advertises {model}"))?;
        }

        Ok(())
    }

    pub async fn models_advertised(
        &mut self,
        organization: OrganizationId,
        runtime: &str,
    ) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT model
             FROM runtime_model
             WHERE organization_id = ? AND runtime = ?
             ORDER BY model",
        )
        .bind(organization.to_string())
        .bind(runtime)
        .fetch_all(&mut *self.transaction)
        .await?;

        Ok(rows.iter().map(|row| row.get("model")).collect())
    }

    pub async fn record_environment(&mut self, run: &Run, environment: &str) -> Result<()> {
        sqlx::query("UPDATE run SET environment = ? WHERE id = ?")
            .bind(environment)
            .bind(run.id.to_string())
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("recording the environment run {} executes in", run.id))?;

        Ok(())
    }

    /// Only a Run that is active holds one, so a heartbeat arriving after its Run ended puts
    /// no lease back.
    pub async fn hold_lease(&mut self, run: &Run, until: Timestamp) -> Result<()> {
        sqlx::query("UPDATE run SET lease_expires_at = ? WHERE id = ? AND state = ?")
            .bind(due(until))
            .bind(run.id.to_string())
            .bind(RunState::Active.as_str())
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("holding the lease of run {} until {until}", run.id))?;

        Ok(())
    }

    pub async fn expired_leases(&mut self, at: Timestamp) -> Result<Vec<Run>> {
        sqlx::query(runs_where!(
            "lease_expires_at <= ? ORDER BY lease_expires_at"
        ))
        .bind(due(at))
        .fetch_all(&mut *self.transaction)
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
                .execute(&mut *self.transaction)
                .await
                .with_context(|| format!("recording the run {} started", run.id))?;

        Ok(started.rows_affected() > 0)
    }

    /// Read and write under the same write lock every `Tx` takes up front, so the check and
    /// whatever the report changes commit together or not at all (ADR-0004).
    pub async fn take_report(&mut self, run: &Run, seq: i64) -> Result<Taken> {
        let taken: i64 = sqlx::query("SELECT reports_taken FROM run WHERE id = ?")
            .bind(run.id.to_string())
            .fetch_one(&mut *self.transaction)
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
            .execute(&mut *self.transaction)
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
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("ending the run {}", run.id))?;

        Ok(ended.rows_affected() > 0)
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
        .execute(&mut *self.transaction)
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
        .execute(&mut *self.transaction)
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
        .fetch_optional(&mut *self.transaction)
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

    pub async fn hold_provider_credential(
        &mut self,
        organization: OrganizationId,
        variable: &str,
        secret: &str,
    ) -> Result<()> {
        let sealed = self
            .keyring
            .seal(&bound_to(organization, variable), secret)?;

        sqlx::query(
            "INSERT INTO provider_credential (organization_id, variable, sealed, set_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (organization_id, variable)
             DO UPDATE SET sealed = excluded.sealed, set_at = excluded.set_at",
        )
        .bind(organization.to_string())
        .bind(variable)
        .bind(sealed)
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("holding the provider credential {variable}"))?;

        Ok(())
    }

    pub async fn provider_credentials_held(
        &mut self,
        organization: OrganizationId,
    ) -> Result<Vec<Held>> {
        sqlx::query(
            "SELECT variable, set_at
             FROM provider_credential
             WHERE organization_id = ?
             ORDER BY variable",
        )
        .bind(organization.to_string())
        .fetch_all(&mut *self.transaction)
        .await?
        .iter()
        .map(|row| {
            Ok(Held {
                variable: row.get("variable"),
                set_at: row.get::<String, _>("set_at").parse()?,
            })
        })
        .collect()
    }

    /// The one place a Provider Credential is decrypted.
    pub async fn provider_credentials(
        &mut self,
        organization: OrganizationId,
    ) -> Result<BTreeMap<String, String>> {
        sqlx::query(
            "SELECT variable, sealed
             FROM provider_credential
             WHERE organization_id = ?
             ORDER BY variable",
        )
        .bind(organization.to_string())
        .fetch_all(&mut *self.transaction)
        .await?
        .iter()
        .map(|row| {
            let variable: String = row.get("variable");
            let secret = self
                .keyring
                .unseal(&bound_to(organization, &variable), row.get("sealed"))
                .with_context(|| format!("opening the provider credential {variable}"))?;

            Ok((variable, secret))
        })
        .collect()
    }

    pub async fn forget_provider_credential(
        &mut self,
        organization: OrganizationId,
        variable: &str,
    ) -> Result<bool> {
        let forgotten = sqlx::query(
            "DELETE FROM provider_credential WHERE organization_id = ? AND variable = ?",
        )
        .bind(organization.to_string())
        .bind(variable)
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("forgetting the provider credential {variable}"))?;

        Ok(forgotten.rows_affected() > 0)
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
        .fetch_one(&mut *self.transaction)
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
        .fetch_all(&mut *self.transaction)
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

    #[expect(
        clippy::too_many_arguments,
        reason = "an integration is what it is declared with"
    )]
    pub async fn register_integration(
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
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("registering the integration {name}"))?;

        Ok(integration)
    }

    pub async fn integrations(&mut self, organization: &Organization) -> Result<Vec<Integration>> {
        sqlx::query(integrations_where!("organization_id = ? ORDER BY name"))
            .bind(organization.id.to_string())
            .fetch_all(&mut *self.transaction)
            .await?
            .iter()
            .map(integration)
            .collect()
    }

    /// Only an Integration that carries events inbound is polled: the direction it declares
    /// is what it does, rather than a label beside it.
    pub async fn integrations_due(&mut self, at: Timestamp) -> Result<Vec<Integration>> {
        sqlx::query(integrations_where!(
            "inbound = TRUE AND poll_due_at <= ? ORDER BY poll_due_at"
        ))
        .bind(due(at))
        .fetch_all(&mut *self.transaction)
        .await
        .context("reading which integrations are due a poll")?
        .iter()
        .map(integration)
        .collect()
    }

    /// `false` when the Event was recorded by an earlier poll whose window overlapped this
    /// one: an Event is identified by what the external system calls it, and recorded once.
    pub async fn record_event(
        &mut self,
        integration: &Integration,
        occurrence: &Occurrence,
    ) -> Result<bool> {
        let recorded = sqlx::query(
            "INSERT INTO event
                 (id, organization_id, integration_id, external_id, repository, kind, actor,
                  subject, title, url, label, message, occurred_at, recorded_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (integration_id, external_id) DO NOTHING",
        )
        .bind(EventId::generate().to_string())
        .bind(integration.organization.to_string())
        .bind(integration.id.to_string())
        .bind(&occurrence.external_id)
        .bind(&integration.repository)
        .bind(&occurrence.kind)
        .bind(&occurrence.actor)
        .bind(occurrence.subject)
        .bind(&occurrence.title)
        .bind(&occurrence.url)
        .bind(occurrence.label.as_deref())
        .bind(occurrence.message.as_deref())
        .bind(occurrence.occurred_at.to_string())
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.transaction)
        .await
        .with_context(|| {
            format!(
                "recording the event {} on {}",
                occurrence.external_id, integration.repository
            )
        })?;

        Ok(recorded.rows_affected() > 0)
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
            .execute(&mut *self.transaction)
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
            .execute(&mut *self.transaction)
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
            "SELECT id, organization_id, integration_id, external_id, repository, kind, actor,
                    subject, title, url, label, message, occurred_at, recorded_at
             FROM event
             WHERE organization_id = ?
             ORDER BY occurred_at DESC, external_id DESC
             LIMIT ?",
        )
        .bind(organization.id.to_string())
        .bind(i64::try_from(limit)?)
        .fetch_all(&mut *self.transaction)
        .await?
        .iter()
        .map(event)
        .collect()
    }

    pub async fn event(&mut self, id: EventId) -> Result<Event> {
        let row = sqlx::query(
            "SELECT id, organization_id, integration_id, external_id, repository, kind, actor,
                    subject, title, url, label, message, occurred_at, recorded_at
             FROM event
             WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&mut *self.transaction)
        .await?
        .with_context(|| format!("no event {id}"))?;

        event(&row)
    }

    pub async fn unfollowed(&mut self, kind: &str, limit: usize) -> Result<Vec<Event>> {
        sqlx::query(
            "SELECT event.id, event.organization_id, event.integration_id, event.external_id,
                    event.repository, event.kind, event.actor, event.subject, event.title,
                    event.url, event.label, event.message, event.occurred_at, event.recorded_at
             FROM event
             LEFT JOIN follow_up ON follow_up.event_id = event.id
             WHERE event.kind = ? AND follow_up.event_id IS NULL
             ORDER BY event.occurred_at, event.external_id
             LIMIT ?",
        )
        .bind(kind)
        .bind(i64::try_from(limit)?)
        .fetch_all(&mut *self.transaction)
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
               AND origin.repository = ?
               AND origin.subject = ?
               AND origin.occurred_at < ?
             ORDER BY session.opened_at DESC, session.id DESC
             LIMIT 1",
        )
        .bind(event.organization.to_string())
        .bind(event.integration.to_string())
        .bind(&event.repository)
        .bind(event.occurrence.subject)
        .bind(event.occurrence.occurred_at.to_string())
        .fetch_optional(&mut *self.transaction)
        .await?;

        match found {
            Some(row) => Ok(Some(
                self.session(row.get::<String, _>("id").parse()?).await?,
            )),
            None => Ok(None),
        }
    }

    pub async fn record_follow_up(
        &mut self,
        event: &Event,
        session: Option<&Session>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO follow_up (event_id, organization_id, session_id, received_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(event.id.to_string())
        .bind(event.organization.to_string())
        .bind(session.map(|session| session.id.to_string()))
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("recording the follow-up event {}", event.id))?;

        Ok(())
    }

    pub async fn integration_with_id(&mut self, id: IntegrationId) -> Result<Integration> {
        let row = sqlx::query(integrations_where!("id = ?"))
            .bind(id.to_string())
            .fetch_optional(&mut *self.transaction)
            .await?
            .with_context(|| format!("no integration {id}"))?;

        integration(&row)
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
        .bind(event.occurrence.subject)
        .bind(body)
        .bind(due(Timestamp::now()))
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.transaction)
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
        .fetch_all(&mut *self.transaction)
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
            .execute(&mut *self.transaction)
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
        .execute(&mut *self.transaction)
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
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("deferring the outcome of run {}", outcome.run))?;

        Ok(())
    }

    pub async fn declare_trigger(
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
        .execute(&mut *self.transaction)
        .await
        .with_context(|| format!("declaring the trigger {name}"))?;

        Ok(trigger)
    }

    pub async fn triggers(&mut self, organization: &Organization) -> Result<Vec<Trigger>> {
        let rows = sqlx::query(triggers_where!("organization_id = ? ORDER BY name"))
            .bind(organization.id.to_string())
            .fetch_all(&mut *self.transaction)
            .await
            .context("reading an organization's triggers")?;

        let mut triggers = Vec::with_capacity(rows.len());
        for row in &rows {
            triggers.push(self.trigger(row).await?);
        }

        Ok(triggers)
    }

    pub async fn trigger_named(
        &mut self,
        organization: &Organization,
        name: &str,
    ) -> Result<Trigger> {
        let row = sqlx::query(triggers_where!("organization_id = ? AND name = ?"))
            .bind(organization.id.to_string())
            .bind(name)
            .fetch_optional(&mut *self.transaction)
            .await?
            .with_context(|| {
                format!(
                    "no trigger named {name} in the organization {}",
                    organization.name
                )
            })?;

        self.trigger(&row).await
    }

    pub async fn set_trigger_state(
        &mut self,
        trigger: &Trigger,
        state: TriggerState,
    ) -> Result<Trigger> {
        sqlx::query("UPDATE trigger SET state = ? WHERE id = ?")
            .bind(state.as_str())
            .bind(trigger.id.to_string())
            .execute(&mut *self.transaction)
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
        .fetch_all(&mut *self.transaction)
        .await
        .context("reading which events a trigger has yet to fire for")?;

        let mut matched = Vec::with_capacity(rows.len());
        for row in &rows {
            let trigger = self
                .trigger_with_id(row.get::<String, _>("trigger_id").parse()?)
                .await?;
            let event = self
                .event(row.get::<String, _>("event_id").parse()?)
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
        .execute(&mut *self.transaction)
        .await
        .with_context(|| {
            format!(
                "recording that the trigger {} fired for the event {}",
                trigger.name, event.id
            )
        })?;

        Ok(())
    }

    async fn trigger_with_id(&mut self, id: TriggerId) -> Result<Trigger> {
        let row = sqlx::query(triggers_where!("id = ?"))
            .bind(id.to_string())
            .fetch_optional(&mut *self.transaction)
            .await?
            .with_context(|| format!("no trigger {id}"))?;

        self.trigger(&row).await
    }

    async fn trigger(&mut self, row: &SqliteRow) -> Result<Trigger> {
        let organization = self
            .organization_with_id(row.get::<String, _>("organization_id").parse()?)
            .await?;
        let workspace = self
            .workspace_with_id(&organization, row.get::<String, _>("workspace_id").parse()?)
            .await?;
        let agent = self
            .agent_with_id(&organization, row.get::<String, _>("agent_id").parse()?)
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

    pub async fn agents(&mut self, organization: &Organization) -> Result<Vec<Agent>> {
        sqlx::query(
            "SELECT id, organization_id, name, runtime, model
             FROM agent
             WHERE organization_id = ?
             ORDER BY name",
        )
        .bind(organization.id.to_string())
        .fetch_all(&mut *self.transaction)
        .await?
        .iter()
        .map(agent)
        .collect()
    }
}

fn workspaces(rows: &[SqliteRow], organization: &Organization) -> Result<Vec<Workspace>> {
    let mut workspaces: Vec<Workspace> = Vec::new();

    for row in rows {
        let id: WorkspaceId = row.get::<String, _>("id").parse()?;
        if workspaces.last().is_none_or(|last| last.id != id) {
            workspaces.push(Workspace {
                id,
                organization: organization.id,
                name: row.get("name"),
                repositories: Vec::new(),
                branch: row.get("branch"),
            });
        }
        if let Some(url) = row.get::<Option<String>, _>("url") {
            workspaces
                .last_mut()
                .expect("the workspace this repository belongs to was just pushed")
                .repositories
                .push(url);
        }
    }

    Ok(workspaces)
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
        repository: row.get("repository"),
        occurrence: Occurrence {
            external_id: row.get("external_id"),
            kind: row.get("kind"),
            actor: row.get("actor"),
            subject: row.get("subject"),
            title: row.get("title"),
            url: row.get("url"),
            label: row.get("label"),
            message: row.get("message"),
            occurred_at: row.get::<String, _>("occurred_at").parse()?,
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

fn agent(row: &SqliteRow) -> Result<Agent> {
    Ok(Agent {
        id: row.get::<String, _>("id").parse()?,
        organization: row.get::<String, _>("organization_id").parse()?,
        name: row.get("name"),
        runtime: row.get("runtime"),
        model: row.get("model"),
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

/// A due time is the one timestamp SQL compares rather than reads back, and at the precision
/// jiff prints by default a whole second sorts after the fractions of it.
fn due(at: Timestamp) -> String {
    format!("{at:.9}")
}

fn timestamp(row: &SqliteRow, column: &str) -> Result<Option<Timestamp>> {
    row.get::<Option<String>, _>(column)
        .map(|at| at.parse())
        .transpose()
        .map_err(Into::into)
}

/// What a sealed credential is authenticated against, so one moved to another organization's
/// row, or to another variable's, no longer opens.
fn bound_to(organization: OrganizationId, variable: &str) -> String {
    format!("{organization}/{variable}")
}

fn organization(row: &SqliteRow) -> Result<Organization> {
    Ok(Organization {
        id: row.get::<String, _>("id").parse()?,
        name: row.get("name"),
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::log::Entry;

    async fn declared(store: &Store) -> (Organization, Workspace, Agent) {
        let mut tx = store.begin().await.unwrap();
        let organization = tx.declare_organization("acme").await.unwrap();
        let workspace = tx
            .declare_workspace(
                &organization,
                "kestrel",
                &["https://github.com/jtmthf/kestrel".to_owned()],
                "main",
            )
            .await
            .unwrap();
        let agent = tx
            .declare_agent(&organization, "builder", "opencode", Some("claude-opus-5"))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        (organization, workspace, agent)
    }

    #[test]
    fn a_due_time_at_a_whole_second_sorts_before_the_moments_after_it() {
        let whole: Timestamp = "2026-09-01T12:00:00Z".parse().unwrap();
        let after: Timestamp = "2026-09-01T12:00:00.5Z".parse().unwrap();

        assert!(due(whole) < due(after));
        assert_eq!(due(whole).parse::<Timestamp>().unwrap(), whole);
    }

    async fn a_run(store: &Store) -> Run {
        let (organization, workspace, agent) = declared(store).await;

        let mut tx = store.begin().await.unwrap();
        let session = tx
            .open_session(&organization, &workspace, &agent, None, None)
            .await
            .unwrap();
        let run = tx.enqueue_run(&session).await.unwrap();
        tx.commit().await.unwrap();

        run
    }

    async fn entries(store: &Store) -> i64 {
        let mut tx = store.begin().await.unwrap();
        sqlx::query("SELECT COUNT(*) AS entries FROM transcript_entry")
            .fetch_one(&mut *tx.transaction)
            .await
            .unwrap()
            .get("entries")
    }

    #[tokio::test]
    async fn a_report_is_taken_once_and_a_replay_of_it_changes_nothing() {
        let data_dir = TempDir::new().unwrap();
        let store = Store::open(data_dir.path()).await.unwrap();
        let run = a_run(&store).await;

        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.take_report(&run, 1).await.unwrap(),
            Taken::Next
        ));
        assert!(matches!(
            tx.take_report(&run, 1).await.unwrap(),
            Taken::Again
        ));
        assert!(matches!(
            tx.take_report(&run, 3).await.unwrap(),
            Taken::Skipped
        ));
        assert!(matches!(
            tx.take_report(&run, 2).await.unwrap(),
            Taken::Next
        ));
    }

    #[tokio::test]
    async fn a_report_taken_in_a_transaction_that_rolls_back_is_the_next_one_again() {
        let data_dir = TempDir::new().unwrap();
        let store = Store::open(data_dir.path()).await.unwrap();
        let run = a_run(&store).await;
        let session = store
            .begin()
            .await
            .unwrap()
            .session(run.session)
            .await
            .unwrap();

        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.take_report(&run, 1).await.unwrap(),
            Taken::Next
        ));
        tx.log()
            .append(
                &session,
                Entry::Said {
                    participant: "builder".to_owned(),
                    message: "lost with the answer to it".to_owned(),
                },
            )
            .await
            .unwrap();
        drop(tx);

        assert_eq!(entries(&store).await, 0);
        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.take_report(&run, 1).await.unwrap(),
            Taken::Next
        ));
    }

    #[tokio::test]
    async fn a_transaction_that_fails_part_way_leaves_neither_the_session_nor_its_entry() {
        let data_dir = TempDir::new().unwrap();
        let store = Store::open(data_dir.path()).await.unwrap();
        let (organization, workspace, agent) = declared(&store).await;

        let mut tx = store.begin().await.unwrap();
        let session = tx
            .open_session(&organization, &workspace, &agent, None, None)
            .await
            .unwrap();
        tx.log()
            .append(
                &session,
                Entry::ParticipantJoined {
                    participant: agent.name.clone(),
                },
            )
            .await
            .unwrap();
        drop(tx);

        let mut tx = store.begin().await.unwrap();
        assert!(tx.session(session.id).await.is_err());
        let entries: i64 = sqlx::query("SELECT COUNT(*) AS entries FROM transcript_entry")
            .fetch_one(&mut *tx.transaction)
            .await
            .unwrap()
            .get("entries");
        assert_eq!(entries, 0);
    }
}
