use std::path::Path;

use anyhow::{Context as _, Result};
use jiff::Timestamp;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteRow};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::domain::{
    Agent, AgentId, Connected, Cost, Exit, Organization, OrganizationId, Run, RunId, RunState,
    Session, SessionId, SessionState, Usage, Workspace, WorkspaceId,
};
use crate::link::credential::Credential;
use crate::link::{Instruction, SentInstruction};
use crate::log::Log;

const DATABASE: &str = "kestrel.db";

macro_rules! runs_where {
    ($tail:literal) => {
        concat!(
            "SELECT id, organization_id, session_id, state, exit, exit_because, environment,
                    enqueued_at, started_at, ended_at, lease_expires_at, connected_at,
                    supervisor_version, context_used, context_size, cost_amount, cost_currency
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

        Ok(Self { pool })
    }

    /// A `Tx` that is dropped rather than committed rolls back, which is how a read is scoped
    /// too. Every one of them takes the write lock up front: SQLite refuses a deferred
    /// transaction that reads and then writes while another has written, rather than making
    /// it wait its turn.
    pub async fn begin(&self) -> Result<Tx<'_>> {
        Ok(Tx {
            transaction: self.pool.begin_with("BEGIN IMMEDIATE").await?,
        })
    }
}

pub struct Tx<'a> {
    transaction: Transaction<'a, Sqlite>,
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
        model: &str,
    ) -> Result<Agent> {
        let agent = Agent {
            id: AgentId::generate(),
            organization: organization.id,
            name: name.to_owned(),
            runtime: runtime.to_owned(),
            model: model.to_owned(),
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

    pub async fn open_session(
        &mut self,
        organization: &Organization,
        workspace: &Workspace,
        agent: &Agent,
        continues: Option<&Session>,
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
            continued_by: Vec::new(),
        };

        sqlx::query(
            "INSERT INTO session
                 (id, organization_id, workspace_id, agent_id, state, opened_at, continues)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(session.id.to_string())
        .bind(organization.id.to_string())
        .bind(workspace.id.to_string())
        .bind(agent.id.to_string())
        .bind(session.state.as_str())
        .bind(session.opened_at.to_string())
        .bind(session.continues.map(|sealed| sealed.to_string()))
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
            "SELECT organization_id, workspace_id, agent_id, state, opened_at, sealed_at, continues
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
            continued_by: self.continuations(id).await?,
        })
    }

    async fn continuations(&mut self, sealed: SessionId) -> Result<Vec<SessionId>> {
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
            .declare_agent(&organization, "builder", "opencode", "claude-opus-5")
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
            .open_session(&organization, &workspace, &agent, None)
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
            .open_session(&organization, &workspace, &agent, None)
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
