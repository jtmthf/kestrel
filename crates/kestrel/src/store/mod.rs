use std::path::Path;

use anyhow::{Context as _, Result};
use jiff::Timestamp;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteRow};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::domain::{
    Agent, AgentId, Connected, Exit, Organization, OrganizationId, Run, RunId, RunState, Session,
    SessionId, SessionState, Workspace, WorkspaceId,
};
use crate::link::credential::Credential;
use crate::link::{Instruction, SentInstruction};
use crate::log::Log;

const DATABASE: &str = "kestrel.db";

macro_rules! runs_where {
    ($tail:literal) => {
        concat!(
            "SELECT id, organization_id, session_id, state, exit, exit_because, environment,
                    enqueued_at, started_at, ended_at, heartbeat_at, connected_at,
                    supervisor_version
             FROM run
             WHERE ",
            $tail
        )
    };
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
    ) -> Result<Session> {
        let session = Session {
            id: SessionId::generate(),
            organization: organization.clone(),
            workspace: workspace.clone(),
            agent: agent.clone(),
            state: SessionState::Open,
            opened_at: Timestamp::now(),
        };

        sqlx::query(
            "INSERT INTO session (id, organization_id, workspace_id, agent_id, state, opened_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(session.id.to_string())
        .bind(organization.id.to_string())
        .bind(workspace.id.to_string())
        .bind(agent.id.to_string())
        .bind(session.state.as_str())
        .bind(session.opened_at.to_string())
        .execute(&mut *self.transaction)
        .await
        .context("opening a session")?;

        Ok(session)
    }

    pub async fn session(&mut self, id: SessionId) -> Result<Session> {
        let row = sqlx::query(
            "SELECT organization_id, workspace_id, agent_id, state, opened_at
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
        })
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
            heartbeat_at: None,
            connected: None,
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
    /// was queued when the statement began and is active by the time anyone else looks.
    pub async fn claim_run(&mut self) -> Result<Option<Run>> {
        let claimed = sqlx::query(
            "UPDATE run
             SET state = ?, claimed_at = ?
             WHERE id = (
                 SELECT id FROM run WHERE state = ? ORDER BY enqueued_at, id LIMIT 1
             )
             RETURNING id",
        )
        .bind(RunState::Active.as_str())
        .bind(Timestamp::now().to_string())
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

    pub async fn record_environment(&mut self, run: &Run, environment: &str) -> Result<()> {
        sqlx::query("UPDATE run SET environment = ? WHERE id = ?")
            .bind(environment)
            .bind(run.id.to_string())
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("recording the environment run {} executes in", run.id))?;

        Ok(())
    }

    pub async fn record_heartbeat(&mut self, run: &Run) -> Result<()> {
        sqlx::query("UPDATE run SET heartbeat_at = ? WHERE id = ?")
            .bind(Timestamp::now().to_string())
            .bind(run.id.to_string())
            .execute(&mut *self.transaction)
            .await
            .with_context(|| format!("heartbeating the run {}", run.id))?;

        Ok(())
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

    /// `false` when the Run had already ended: whoever ends it first decides its exit status.
    pub async fn end_run(&mut self, run: &Run, exit: &Exit) -> Result<bool> {
        let ended = sqlx::query(
            "UPDATE run
             SET state = ?, ended_at = ?, exit = ?, exit_because = ?
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
        heartbeat_at: timestamp(row, "heartbeat_at")?,
        connected: match connected_at {
            Some(at) => Some(Connected {
                at: at.parse()?,
                version: row.get("supervisor_version"),
            }),
            None => None,
        },
    })
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

    #[tokio::test]
    async fn a_transaction_that_fails_part_way_leaves_neither_the_session_nor_its_entry() {
        let data_dir = TempDir::new().unwrap();
        let store = Store::open(data_dir.path()).await.unwrap();
        let (organization, workspace, agent) = declared(&store).await;

        let mut tx = store.begin().await.unwrap();
        let session = tx
            .open_session(&organization, &workspace, &agent)
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
