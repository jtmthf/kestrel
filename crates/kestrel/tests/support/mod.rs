//! The primary test seam (0.1/03): boot a complete control plane in-process against a fresh
//! temporary SQLite file, drive it through the same paths a person would use, and tear it
//! down. Assertions live in the language of Sessions, Runs and Transcripts; `Store` and `Log`
//! stay behind `Harness`, never reached for directly.

// Every integration-test binary compiles all of this; a helper one of them does not reach for
// is not dead, it belongs to a sibling.
#![allow(dead_code)]

pub mod built;
pub mod compose;
pub mod control_plane;
pub mod diagnostics;
pub mod docker;
pub mod environment;
pub mod github_stub;
pub mod image;
pub mod lineage;
pub mod link_client;
pub mod model;
pub mod repository;
pub mod scripted_agent;
pub mod supervisor;

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::Path;

use jiff::{SignedDuration, Timestamp};
use kestrel::agent;
use kestrel::compute::{Docker, Driver, LocalExec};
use kestrel::domain::{
    Agent, Direction, Event, Integration, IntegrationKind, Organization, Run, RunId, Session,
    SessionId, Trigger, Workspace,
};
use kestrel::integration::{self, Registration};
use kestrel::link::credential::Secret;
use kestrel::link::{self, Instruction};
use kestrel::log::{Cursor, Page, TranscriptEntry, Unreadable, Window};
use kestrel::provider::{self, Held};
use kestrel::role::work::Dispatch;
use kestrel::session;
use kestrel::store::Store;
use kestrel::trigger::{self, Declaration};
use kestrel::work::{self, Claimed};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Distinctive enough that a test can assert it is nowhere it should not be.
pub const TOKEN: &str = "ghp_kestrel_should_never_say_this_out_loud";

/// The Provider Credential every fixture holds: a Run reaches no model without one, and the
/// scripted agent's `Confides` script says it can see this one.
pub const PROVIDER_KEY: &str = "SCRIPTED_API_KEY";
pub const A_PROVIDER_KEY: &str = "a-provider-key";

pub struct Harness {
    data_dir: TempDir,
    store: Store,
    address: SocketAddr,
    environment: Option<Provisions>,
    shutdown: CancellationToken,
    roles: JoinHandle<anyhow::Result<()>>,
}

/// What the work role provisions an Environment with.
#[derive(Clone)]
pub struct Provisions {
    driver: Driver,
    runtime: String,
    max_active_runs: NonZeroUsize,
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

    /// Bound on every interface rather than on loopback, because what dials this one is a
    /// container and reaches this machine by its gateway address.
    pub async fn boot_reachable_from_an_environment() -> Self {
        let data_dir = TempDir::new().expect("a temporary data directory");
        Self::boot_against(
            data_dir,
            "0.0.0.0:0".parse().expect("every interface"),
            None,
        )
        .await
    }

    pub async fn dispatching(supervisor: &Path) -> Self {
        Self::dispatching_to(
            supervisor,
            &scripted_agent::playing(scripted_agent::Script::Speaks),
        )
        .await
    }

    pub async fn dispatching_to(supervisor: &Path, runtime: &str) -> Self {
        Self::dispatching_up_to(supervisor, runtime, 2).await
    }

    pub async fn dispatching_up_to(supervisor: &Path, runtime: &str, maximum: usize) -> Self {
        Self::booted(Some(Provisions {
            driver: Driver::LocalExec(LocalExec::running(supervisor)),
            runtime: runtime.to_owned(),
            max_active_runs: NonZeroUsize::new(maximum).expect("at least one active run"),
        }))
        .await
    }

    /// The Docker driver, on a control plane bound where a container can dial out to it.
    pub async fn dispatching_in(image: &str, runtime: &str) -> Self {
        let data_dir = TempDir::new().expect("a temporary data directory");
        Self::boot_against(
            data_dir,
            "0.0.0.0:0".parse().expect("every interface"),
            Some(Provisions {
                driver: Driver::Docker(Docker::provisioning_from(image)),
                runtime: runtime.to_owned(),
                max_active_runs: NonZeroUsize::new(2).unwrap(),
            }),
        )
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
            link: match provisions.driver {
                Driver::Docker(_) => format!("http://host.docker.internal:{}", address.port()),
                Driver::LocalExec(_) => format!("http://{address}"),
            },
            driver: provisions.driver,
            runtime: provisions.runtime,
            auth: None,
            max_active_runs: provisions.max_active_runs,
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

    pub fn link_from_an_environment(&self) -> String {
        format!("http://host.docker.internal:{}", self.address.port())
    }

    pub async fn declare_organization(&self, name: &str) -> Organization {
        let mut tx = self.store.begin().await.expect("a transaction");
        let organization = tx
            .organizations()
            .declare(name)
            .await
            .expect("the organization should declare");
        tx.commit().await.expect("the declaration should commit");
        organization
    }

    pub async fn organizations(&self) -> Vec<Organization> {
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.organizations()
            .all()
            .await
            .expect("organizations should list")
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
            .workspaces()
            .declare(organization, name, repositories, branch)
            .await
            .expect("the workspace should declare");
        tx.commit().await.expect("the declaration should commit");
        workspace
    }

    pub async fn workspaces(&self, organization: &Organization) -> Vec<Workspace> {
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.workspaces()
            .all(organization)
            .await
            .expect("workspaces should list")
    }

    pub async fn declare_agent(
        &self,
        organization: &Organization,
        name: &str,
        runtime: &str,
        model: Option<&str>,
    ) -> Agent {
        self.try_declare_agent(organization, name, runtime, model)
            .await
            .expect("the agent should declare")
    }

    pub async fn try_declare_agent(
        &self,
        organization: &Organization,
        name: &str,
        runtime: &str,
        model: Option<&str>,
    ) -> anyhow::Result<Agent> {
        agent::declare(&self.store, &organization.name, name, runtime, model).await
    }

    pub async fn set_agent_model(
        &self,
        organization: &Organization,
        name: &str,
        model: Option<&str>,
    ) -> Agent {
        agent::set_model(&self.store, &organization.name, name, model)
            .await
            .expect("the model should change")
    }

    pub async fn agents(&self, organization: &Organization) -> Vec<Agent> {
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.agents()
            .all(organization)
            .await
            .expect("agents should list")
    }

    pub async fn register_integration(
        &self,
        organization: &str,
        name: &str,
        repository: &str,
        api: &str,
        carries: &[Direction],
        interval: SignedDuration,
    ) -> Integration {
        self.try_register_integration(organization, name, repository, api, carries, interval)
            .await
            .expect("the integration should register")
    }

    pub async fn try_register_integration(
        &self,
        organization: &str,
        name: &str,
        repository: &str,
        api: &str,
        carries: &[Direction],
        interval: SignedDuration,
    ) -> anyhow::Result<Integration> {
        integration::register(
            &self.store,
            Registration {
                organization,
                name,
                kind: IntegrationKind::Github,
                repository,
                api,
                token: TOKEN,
                carries,
                interval,
            },
        )
        .await
    }

    pub async fn integrations(&self, organization: &str) -> Vec<Integration> {
        integration::integrations(&self.store, organization)
            .await
            .expect("the integrations should list")
    }

    pub async fn events(&self, organization: &str) -> Vec<Event> {
        integration::events(&self.store, organization, 100)
            .await
            .expect("the events should list")
    }

    pub async fn declare_trigger(
        &self,
        organization: &str,
        name: &str,
        matching: (&str, &str),
        workspace: &str,
        agent: &str,
    ) -> Trigger {
        self.try_declare_trigger(organization, name, matching, workspace, agent)
            .await
            .expect("the trigger should declare")
    }

    pub async fn try_declare_trigger(
        &self,
        organization: &str,
        name: &str,
        matching: (&str, &str),
        workspace: &str,
        agent: &str,
    ) -> anyhow::Result<Trigger> {
        let (repository, label) = matching;
        trigger::declare(
            &self.store,
            Declaration {
                organization,
                name,
                repository,
                label,
                workspace,
                agent,
            },
        )
        .await
    }

    pub async fn triggers(&self, organization: &str) -> Vec<Trigger> {
        trigger::triggers(&self.store, organization)
            .await
            .expect("the triggers should list")
    }

    pub async fn show_trigger(&self, organization: &str, name: &str) -> Trigger {
        trigger::show(&self.store, organization, name)
            .await
            .expect("the trigger should show")
    }

    pub async fn disable_trigger(&self, organization: &str, name: &str) -> Trigger {
        trigger::disable(&self.store, organization, name)
            .await
            .expect("the trigger should disable")
    }

    pub async fn enable_trigger(&self, organization: &str, name: &str) -> Trigger {
        trigger::enable(&self.store, organization, name)
            .await
            .expect("the trigger should enable")
    }

    pub async fn hold_provider_credential(
        &self,
        organization: &Organization,
        variable: &str,
        secret: &str,
    ) {
        provider::hold(&self.store, &organization.name, variable, secret)
            .await
            .expect("the provider credential should be held");
    }

    pub async fn provider_credentials_held(&self, organization: &Organization) -> Vec<Held> {
        provider::held(&self.store, &organization.name)
            .await
            .expect("what the organization holds should list")
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

    pub async fn sessions(&self, organization: &str) -> Vec<Session> {
        session::sessions(&self.store, organization)
            .await
            .expect("the sessions should list")
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

    pub async fn post(&self, id: SessionId, participant: &str, message: &str) -> Run {
        self.post_while_busy(id, participant, message)
            .await
            .expect("an idle session should enqueue a run")
    }

    pub async fn post_while_busy(
        &self,
        id: SessionId,
        participant: &str,
        message: &str,
    ) -> Option<Run> {
        session::post(&self.store, id, participant, message)
            .await
            .expect("the message should post")
    }

    pub async fn has_pending_messages(&self, id: SessionId) -> bool {
        let mut tx = self.store.begin().await.expect("a transaction");
        let session = tx
            .sessions()
            .get(id)
            .await
            .expect("the session should read");
        tx.sessions()
            .has_pending_messages(&session)
            .await
            .expect("pending messages should read")
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

    pub async fn block_run(&self, run: &Run, blocker: &Run) {
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.sessions()
            .declare_blocked(run, blocker)
            .await
            .expect("the run should be declared blocked");
        tx.commit().await.expect("the declaration should commit");
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

    pub async fn fail_run(&self, run: &Run, because: &str) {
        work::fail(&self.store, run, because)
            .await
            .expect("the run should end");
    }

    /// The `ended`/NULL row migration 0003 leaves behind for every Run predating kestrel
    /// scheduling. `end_run` always records an exit, so nothing reachable through the store
    /// produces one.
    pub async fn end_run_without_an_exit(&self, run: &Run) {
        let database = self.data_dir().join("kestrel.db");
        let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", database.display()))
            .await
            .expect("the database should open");

        sqlx::query("UPDATE run SET state = 'ended', ended_at = ?, exit = NULL WHERE id = ?")
            .bind(jiff::Timestamp::now().to_string())
            .bind(run.id.to_string())
            .execute(&pool)
            .await
            .expect("the run should end without an exit");

        pool.close().await;
    }

    /// Backdates when Events were recorded, the way `lease_until` backdates a lease: the only
    /// way to watch the reaping sweep without waiting the retention window out.
    pub async fn backdate_events(&self, to: &Timestamp) {
        let database = self.data_dir().join("kestrel.db");
        let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", database.display()))
            .await
            .expect("the database should open");

        sqlx::query("UPDATE event SET recorded_at = ?")
            .bind(to.to_string())
            .execute(&pool)
            .await
            .expect("the events should backdate");

        pool.close().await;
    }

    pub async fn environment_present(&self, run: &Run, environment: &str) {
        work::environment_present(&self.store, run, environment)
            .await
            .expect("the environment should be recorded");
    }

    pub async fn environments_to_reap(&self) -> Vec<(Run, String)> {
        work::environments_to_reap(&self.store)
            .await
            .expect("ended environments should read")
    }

    pub async fn environment_gone(&self, run: &Run) {
        work::environment_gone(&self.store, run)
            .await
            .expect("the environment should be gone");
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
        tx.sessions()
            .hold_lease(run, expires_at)
            .await
            .expect("the lease should hold");
        tx.commit().await.expect("the lease should commit");
    }

    /// Backdates when a Session was last active, the way `lease_until` backdates a lease: the
    /// only way to watch the idle sweep without waiting the window out.
    pub async fn last_active(&self, session: &Session, at: Timestamp) {
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.sessions()
            .record_active(session.id, at)
            .await
            .expect("the session should record when it was last active");
        tx.commit().await.expect("the record should commit");
    }

    /// A second credential for the same Run, with an expiry the caller chooses. The only way
    /// to hold an expired one without waiting out a real credential's life.
    pub async fn issue_credential(&self, run: &Run, expires_at: Timestamp) -> Secret {
        let secret = Secret::mint();
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.sessions()
            .issue_credential(run, &secret.digest(), expires_at)
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
        tx.sessions()
            .hold_lease(run, expires_at)
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
