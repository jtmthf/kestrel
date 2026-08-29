//! The primary test seam (0.1/03): boot a complete control plane in-process against a fresh
//! temporary SQLite file, drive it through the same paths a person would use, and tear it
//! down. Assertions live in the language of Sessions, Runs and Transcripts; `Store` and `Log`
//! stay behind `Harness`, never reached for directly.

pub mod github_stub;
pub mod scripted_runtime;

use std::path::Path;

use kestrel::domain::{Agent, Organization, Session, SessionId, Workspace};
use kestrel::log::TranscriptEntry;
use kestrel::session;
use kestrel::store::Store;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub struct Harness {
    data_dir: TempDir,
    store: Store,
    shutdown: CancellationToken,
    roles: JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    pub async fn boot() -> Self {
        let data_dir = TempDir::new().expect("a temporary data directory");
        Self::boot_against(data_dir).await
    }

    async fn boot_against(data_dir: TempDir) -> Self {
        let store = Store::open(data_dir.path())
            .await
            .expect("the control plane should boot against a fresh data directory");
        let shutdown = CancellationToken::new();
        let roles = tokio::spawn(kestrel::role::all_in_one(shutdown.clone()));

        Self {
            data_dir,
            store,
            shutdown,
            roles,
        }
    }

    pub fn data_dir(&self) -> &Path {
        self.data_dir.path()
    }

    pub async fn declare_organization(&self, name: &str) -> Organization {
        let mut tx = self.store.begin().await.expect("a transaction");
        let organization = tx
            .declare_organization(name)
            .await
            .expect("the organization should declare");
        tx.commit().await.expect("the declaration should commit");
        organization
    }

    pub async fn organizations(&self) -> Vec<Organization> {
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.organizations().await.expect("organizations should list")
    }

    pub async fn declare_workspace(
        &self,
        organization: &Organization,
        name: &str,
        repositories: &[String],
        branch: &str,
    ) -> Workspace {
        let mut tx = self.store.begin().await.expect("a transaction");
        let workspace = tx
            .declare_workspace(organization, name, repositories, branch)
            .await
            .expect("the workspace should declare");
        tx.commit().await.expect("the declaration should commit");
        workspace
    }

    pub async fn workspaces(&self, organization: &Organization) -> Vec<Workspace> {
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.workspaces(organization)
            .await
            .expect("workspaces should list")
    }

    pub async fn declare_agent(
        &self,
        organization: &Organization,
        name: &str,
        runtime: &str,
        model: &str,
    ) -> Agent {
        let mut tx = self.store.begin().await.expect("a transaction");
        let agent = tx
            .declare_agent(organization, name, runtime, model)
            .await
            .expect("the agent should declare");
        tx.commit().await.expect("the declaration should commit");
        agent
    }

    pub async fn agents(&self, organization: &Organization) -> Vec<Agent> {
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.agents(organization).await.expect("agents should list")
    }

    pub async fn open_session(&self, organization: &str, workspace: &str, agent: &str) -> Session {
        session::open(&self.store, organization, workspace, agent)
            .await
            .expect("the session should open")
    }

    pub async fn show_session(&self, id: SessionId) -> Session {
        session::show(&self.store, id)
            .await
            .expect("the session should show")
    }

    pub async fn transcript(&self, id: SessionId) -> Vec<TranscriptEntry> {
        session::transcript(&self.store, id)
            .await
            .expect("the transcript should read")
    }

    /// Simulates `kill -9`: aborts the roles and drops the store with no graceful shutdown,
    /// then boots a fresh control plane against the same data directory.
    pub async fn kill_and_restart(self) -> Self {
        self.roles.abort();
        let _ = self.roles.await;
        drop(self.store);

        Self::boot_against(self.data_dir).await
    }

    pub async fn teardown(self) {
        self.shutdown.cancel();
        let _ = self.roles.await;
    }
}
