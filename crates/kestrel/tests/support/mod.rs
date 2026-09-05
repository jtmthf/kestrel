//! The primary test seam (0.1/03): boot a complete control plane in-process against a fresh
//! temporary SQLite file, drive it through the same paths a person would use, and tear it
//! down. Assertions live in the language of Sessions, Runs and Transcripts; `Store` and `Log`
//! stay behind `Harness`, never reached for directly.

// Every integration-test binary compiles all of this; a helper one of them does not reach for
// is not dead, it belongs to a sibling.
#![allow(dead_code)]

pub mod built;
pub mod environment;
pub mod github_stub;
pub mod link_client;
pub mod scripted_agent;
pub mod supervisor;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use kestrel::domain::{Agent, Organization, Run, RunId, Session, SessionId, Workspace};
use kestrel::link::credential::Secret;
use kestrel::link::{self, Instruction};
use kestrel::log::{Cursor, Page, TranscriptEntry, Unreadable, Window};
use kestrel::role::work::Dispatch;
use kestrel::session;
use kestrel::store::Store;
use kestrel::work::{self, Claimed};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub struct Harness {
    data_dir: TempDir,
    store: Store,
    address: SocketAddr,
    environment: Option<Provisions>,
    shutdown: CancellationToken,
    roles: JoinHandle<anyhow::Result<()>>,
}

/// What an Environment the work role provisions runs.
#[derive(Clone)]
pub struct Provisions {
    supervisor: PathBuf,
    runtime: String,
}

/// Comes back on the address it was listening on, so what an Environment already dialled
/// still reaches it.
pub struct Stopped {
    data_dir: TempDir,
    address: SocketAddr,
    environment: Option<Provisions>,
}

impl Harness {
    /// Boots with no supervisor to provision an Environment with, so the work role claims
    /// nothing and a test is the only thing dispatching the Runs it opens.
    pub async fn boot() -> Self {
        Self::booted(None).await
    }

    pub async fn dispatching(supervisor: &Path) -> Self {
        Self::dispatching_to(
            supervisor,
            &scripted_agent::playing(scripted_agent::Script::Speaks),
        )
        .await
    }

    pub async fn dispatching_to(supervisor: &Path, runtime: &str) -> Self {
        Self::booted(Some(Provisions {
            supervisor: supervisor.to_path_buf(),
            runtime: runtime.to_owned(),
        }))
        .await
    }

    async fn booted(environment: Option<Provisions>) -> Self {
        let data_dir = TempDir::new().expect("a temporary data directory");
        Self::boot_against(
            data_dir,
            "127.0.0.1:0".parse().expect("a loopback address"),
            environment,
        )
        .await
    }

    async fn boot_against(
        data_dir: TempDir,
        listen: SocketAddr,
        environment: Option<Provisions>,
    ) -> Self {
        let store = Store::open(data_dir.path())
            .await
            .expect("the control plane should boot against a fresh data directory");
        let shutdown = CancellationToken::new();
        let all_in_one = kestrel::role::bind(store.clone(), listen)
            .await
            .expect("the control plane should bind its link");
        let address = all_in_one.address();
        let dispatch = environment.clone().map(|provisions| Dispatch {
            link: format!("http://{address}"),
            supervisor: provisions.supervisor,
            runtime: provisions.runtime,
        });
        let roles = tokio::spawn(all_in_one.run(dispatch, shutdown.clone()));

        Self {
            data_dir,
            store,
            address,
            environment,
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
        self.try_open_session(organization, workspace, agent, None)
            .await
            .expect("the session should open")
    }

    pub async fn continue_session(
        &self,
        organization: &str,
        workspace: &str,
        agent: &str,
        continues: SessionId,
    ) -> Session {
        self.try_open_session(organization, workspace, agent, Some(continues))
            .await
            .expect("the session should open")
    }

    pub async fn try_open_session(
        &self,
        organization: &str,
        workspace: &str,
        agent: &str,
        continues: Option<SessionId>,
    ) -> anyhow::Result<Session> {
        session::open(&self.store, organization, workspace, agent, continues).await
    }

    pub async fn seal_session(&self, id: SessionId) -> Session {
        self.try_seal_session(id)
            .await
            .expect("the session should seal")
    }

    pub async fn try_seal_session(&self, id: SessionId) -> anyhow::Result<Session> {
        session::seal(&self.store, id).await
    }

    pub async fn continuations(&self, id: SessionId) -> Vec<SessionId> {
        session::continuations(&self.store, id)
            .await
            .expect("the continuations should read")
    }

    pub async fn show_session(&self, id: SessionId) -> Session {
        session::show(&self.store, id)
            .await
            .expect("the session should show")
    }

    pub async fn transcript(&self, id: SessionId) -> Vec<TranscriptEntry> {
        self.walk(id, None, Window::DEFAULT).await
    }

    /// One bounded window is the only read there is, so a whole Transcript is a walk.
    pub async fn walk(
        &self,
        id: SessionId,
        from: Option<Cursor>,
        window: Window,
    ) -> Vec<TranscriptEntry> {
        let mut walked = Vec::new();
        let mut cursor = from;

        loop {
            let page = self
                .page(id, cursor, window)
                .await
                .expect("the transcript should read");
            walked.extend(page.entries);
            cursor = page.cursor;

            if !page.more {
                return walked;
            }
        }
    }

    pub async fn page(
        &self,
        id: SessionId,
        from: Option<Cursor>,
        window: Window,
    ) -> Result<Page, Unreadable> {
        session::transcript(&self.store, id, from, window).await
    }

    pub async fn said(&self, run: &Run, message: &str) {
        self.try_said(run, message)
            .await
            .expect("the message should reach the transcript");
    }

    pub async fn try_said(&self, run: &Run, message: &str) -> anyhow::Result<()> {
        let mut tx = self.store.begin().await.expect("a transaction");
        work::said(&mut tx, run, message).await?;
        tx.commit().await
    }

    pub async fn enqueue_run(&self, session: SessionId) -> Run {
        self.try_enqueue_run(session)
            .await
            .expect("the run should enqueue")
    }

    pub async fn try_enqueue_run(&self, session: SessionId) -> anyhow::Result<Run> {
        work::enqueue(&self.store, session).await
    }

    /// Claims what it enqueued, standing in for the work role a `boot`ed harness leaves idle.
    pub async fn dispatch_run(&self, session: SessionId) -> (Run, Secret) {
        self.enqueue_run(session).await;
        let claimed = self
            .claim_run()
            .await
            .expect("a run was just enqueued to claim");

        (claimed.run, claimed.credential)
    }

    pub async fn claim_run(&self) -> Option<Claimed> {
        work::claim(&self.store)
            .await
            .expect("the claim should ask")
    }

    pub async fn run(&self, id: RunId) -> Run {
        work::run(&self.store, id)
            .await
            .expect("the run should show")
    }

    pub async fn runs(&self, session: SessionId) -> Vec<Run> {
        work::runs(&self.store, session)
            .await
            .expect("the runs should list")
    }

    pub async fn complete_run(&self, run: &Run) {
        work::complete(&self.store, run)
            .await
            .expect("the run should end");
    }

    pub async fn instruct(&self, run: &Run, instruction: Instruction) {
        self.try_instruct(run, instruction)
            .await
            .expect("the instruction should send");
    }

    pub async fn try_instruct(
        &self,
        run: &Run,
        instruction: Instruction,
    ) -> anyhow::Result<link::SentInstruction> {
        link::instruct(&self.store, run, instruction).await
    }

    /// A lease that is up when the caller says rather than when a real one would be. The only
    /// way to watch a sweep without waiting a whole lease out.
    pub async fn lease_until(&self, run: &Run, expires_at: Timestamp) {
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.hold_lease(run, expires_at)
            .await
            .expect("the lease should hold");
        tx.commit().await.expect("the lease should commit");
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
            environment: self.environment,
        }
    }

    pub async fn kill_and_restart(self) -> Self {
        self.kill().await.restart().await
    }

    /// Stops the way a signalled control plane does: every role is told to stop and is waited
    /// for, rather than being cut off where it stood.
    pub async fn teardown(self) -> Stopped {
        self.shutdown.cancel();
        let _ = self.roles.await;
        drop(self.store);

        Stopped {
            data_dir: self.data_dir,
            address: self.address,
            environment: self.environment,
        }
    }
}

impl Stopped {
    pub async fn restart(self) -> Harness {
        Harness::boot_against(self.data_dir, self.address, self.environment).await
    }

    pub async fn run(&self, id: RunId) -> Run {
        let store = Store::open(self.data_dir.path())
            .await
            .expect("the database should still be there");

        work::run(&store, id).await.expect("the run should show")
    }

    /// A due time set while nothing is keeping time, so what fires it afterwards is a control
    /// plane that could only have read it back.
    pub async fn lease_until(&self, run: &Run, expires_at: Timestamp) {
        let store = Store::open(self.data_dir.path())
            .await
            .expect("the database should still be there");
        let mut tx = store.begin().await.expect("a transaction");
        tx.hold_lease(run, expires_at)
            .await
            .expect("the lease should hold");
        tx.commit().await.expect("the lease should commit");
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
