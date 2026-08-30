//! The primary test seam (0.1/03): boot a complete control plane in-process against a fresh
//! temporary SQLite file, drive it through the same paths a person would use, and tear it
//! down. Assertions live in the language of Sessions, Runs and Transcripts; `Store` and `Log`
//! stay behind `Harness`, never reached for directly.

// Every integration-test binary compiles all of this; a helper one of them does not reach for
// is not dead, it belongs to a sibling.
#![allow(dead_code)]

pub mod github_stub;
pub mod link_client;
pub mod scripted_runtime;
pub mod supervisor;

use std::net::SocketAddr;
use std::path::Path;

use jiff::Timestamp;
use kestrel::domain::{Agent, Organization, Run, RunId, Session, SessionId, Workspace};
use kestrel::link::credential::Secret;
use kestrel::link::{self, Instruction};
use kestrel::log::TranscriptEntry;
use kestrel::session;
use kestrel::store::Store;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub struct Harness {
    data_dir: TempDir,
    store: Store,
    address: SocketAddr,
    shutdown: CancellationToken,
    roles: JoinHandle<anyhow::Result<()>>,
}

/// Comes back on the address it was listening on, so what an Environment already dialled
/// still reaches it.
pub struct Stopped {
    data_dir: TempDir,
    address: SocketAddr,
}

impl Harness {
    pub async fn boot() -> Self {
        let data_dir = TempDir::new().expect("a temporary data directory");
        Self::boot_against(data_dir, "127.0.0.1:0".parse().expect("a loopback address")).await
    }

    async fn boot_against(data_dir: TempDir, listen: SocketAddr) -> Self {
        let store = Store::open(data_dir.path())
            .await
            .expect("the control plane should boot against a fresh data directory");
        let shutdown = CancellationToken::new();
        let all_in_one = kestrel::role::bind(store.clone(), listen)
            .await
            .expect("the control plane should bind its link");
        let address = all_in_one.address();
        let roles = tokio::spawn(all_in_one.run(shutdown.clone()));

        Self {
            data_dir,
            store,
            address,
            shutdown,
            roles,
        }
    }

    pub fn data_dir(&self) -> &Path {
        self.data_dir.path()
    }

    pub fn link(&self) -> String {
        format!("http://{}", self.address)
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

    pub async fn start_run(&self, session: SessionId) -> (Run, Secret) {
        session::start_run(&self.store, session)
            .await
            .expect("the run should start")
    }

    pub async fn run(&self, id: RunId) -> Run {
        session::run(&self.store, id)
            .await
            .expect("the run should show")
    }

    pub async fn end_run(&self, run: &Run) {
        session::end_run(&self.store, run)
            .await
            .expect("the run should end");
    }

    pub async fn instruct(&self, run: &Run, instruction: Instruction) {
        link::instruct(&self.store, run, instruction)
            .await
            .expect("the instruction should send");
    }

    /// A second credential for the same Run, with an expiry the caller chooses. The only way
    /// to hold an expired one without waiting out a real credential's life.
    pub async fn issue_credential(&self, run: &Run, expires_at: Timestamp) -> Secret {
        let secret = Secret::mint();
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.issue_credential(run, &secret.digest(), expires_at)
            .await
            .expect("the credential should issue");
        tx.commit().await.expect("the credential should commit");
        secret
    }

    /// Simulates the process going away: every role and every stream it was holding open stops
    /// at once, and the store is dropped rather than closed.
    pub async fn kill(self) -> Stopped {
        self.shutdown.cancel();
        self.roles.abort();
        let _ = self.roles.await;
        drop(self.store);

        Stopped {
            data_dir: self.data_dir,
            address: self.address,
        }
    }

    pub async fn kill_and_restart(self) -> Self {
        self.kill().await.restart().await
    }

    pub async fn teardown(self) {
        self.shutdown.cancel();
        let _ = self.roles.await;
    }
}

impl Stopped {
    pub async fn restart(self) -> Harness {
        Harness::boot_against(self.data_dir, self.address).await
    }

    /// Reaches the durable record while nothing is serving it, which is the only way to make
    /// an instruction that an Environment provably could not have been handed as it was sent.
    pub async fn instruct(&self, run: &Run, instruction: Instruction) {
        let store = Store::open(self.data_dir.path())
            .await
            .expect("the database should still be there");
        link::instruct(&store, run, instruction)
            .await
            .expect("the instruction should send");
    }
}
