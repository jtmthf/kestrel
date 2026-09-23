//! The primary test seam (0.1/03): boot a complete control plane in-process against a fresh
//! temporary SQLite file, drive it through the same paths a person would use, and tear it
//! down. Assertions live in the language of Sessions, Runs and Transcripts; `Store` and `Log`
//! stay behind `Harness`, never reached for directly.

// Every integration-test binary compiles all of this; a helper one of them does not reach for
// is not dead, it belongs to a sibling.
#![allow(dead_code)]

pub mod built;
pub mod client;
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
pub mod operator_log;
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
    Agent, CorrelationMiss, Direction, Event, EventRecordId, Exit, Fires, Integration, Occurrence,
    Organization, Run, RunId, RunState, Schedule, Session, SessionId, SubscriptionProfile,
    Templates, Trigger, Turn, Workspace,
};
use kestrel::instance;
use kestrel::integration::{self, Connecting, Registration};
use kestrel::link::credential::Secret;
use kestrel::link::{self, Instruction};
use kestrel::log::{Cursor, Entry, Page, TranscriptEntry, Unreadable, Window};
use kestrel::profile::{self, Contents};
use kestrel::provider::{self, Held};
use kestrel::role::serve::Listen;
use kestrel::role::work::{AgentRuntime, Dispatch};
use kestrel::session;
use kestrel::store::Store;
use kestrel::trigger::apply::Applied;
use kestrel::trigger::{self, Declaration, Tested};
use kestrel::work::{self, Claimed};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

/// Distinctive enough that a test can assert it is nowhere it should not be.
pub const TOKEN: &str = "ghp_kestrel_should_never_say_this_out_loud";

/// The Provider Credential every fixture holds: a Run reaches no model without one, and the
/// scripted agent's `Confides` script says it can see this one.
pub const PROVIDER_KEY: &str = "SCRIPTED_API_KEY";
pub const A_PROVIDER_KEY: &str = "a-provider-key";

pub fn labelled_on(repository: &str, label: &str) -> String {
    serde_json::json!({"all": [
        {"exact": {"source": format!("https://github.com/{repository}")}},
        {"exact": {"type": "com.github.issues.labeled"}},
        {"exact": {"data.label.name": label}},
    ]})
    .to_string()
}

pub const BRIEF: &str = "Work on {{ event.data.issue.title }}";

pub fn templates(brief: &str, branch: Option<&str>, correlation: Option<&str>) -> Templates {
    let parsed = |template: &str| template.parse().expect("the template should parse");

    Templates {
        brief: parsed(brief),
        branch: branch.map(parsed),
        correlation: correlation.map(parsed),
    }
}

const LOOPBACK: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0);

pub struct Harness {
    data_dir: TempDir,
    store: Store,
    bound: Listen,
    environment: Option<Provisions>,
    shutdown: CancellationToken,
    roles: JoinHandle<anyhow::Result<()>>,
}

/// What the work role provisions an Environment with.
#[derive(Clone)]
pub struct Provisions {
    driver: Driver,
    runtimes: Vec<AgentRuntime>,
    max_active_runs: NonZeroUsize,
}

/// The runtime an Agent names unless a test says otherwise, spawned as whatever the test plays.
pub const RUNTIME: &str = "opencode";
/// The runtime whose Runs on one Subscription Profile the work role dispatches one at a time.
pub const SERIALIZED: &str = "codex";

fn spawning(runtimes: &[(&str, &str)]) -> Vec<AgentRuntime> {
    runtimes
        .iter()
        .map(|&(name, command)| AgentRuntime {
            name: name.to_owned(),
            command: command.to_owned(),
        })
        .collect()
}

/// Comes back on the address it was listening on, so what an Environment already dialled
/// still reaches it.
pub struct Stopped {
    data_dir: TempDir,
    bound: Listen,
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
            Listen {
                link: "0.0.0.0:0".parse().expect("every interface"),
                operator: LOOPBACK,
            },
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
        Self::dispatching_runtimes_up_to(supervisor, &[(RUNTIME, runtime)], maximum).await
    }

    pub async fn dispatching_runtimes(supervisor: &Path, runtimes: &[(&str, &str)]) -> Self {
        Self::dispatching_runtimes_up_to(supervisor, runtimes, 2).await
    }

    pub async fn dispatching_runtimes_up_to(
        supervisor: &Path,
        runtimes: &[(&str, &str)],
        maximum: usize,
    ) -> Self {
        Self::booted(Some(Provisions {
            driver: Driver::LocalExec(LocalExec::running(supervisor)),
            runtimes: spawning(runtimes),
            max_active_runs: NonZeroUsize::new(maximum).expect("at least one active run"),
        }))
        .await
    }

    /// The Docker driver, on a control plane bound where a container can dial out to it.
    pub async fn dispatching_in(image: &str, runtime: &str) -> Self {
        let data_dir = TempDir::new().expect("a temporary data directory");
        Self::boot_against(
            data_dir,
            Listen {
                link: "0.0.0.0:0".parse().expect("every interface"),
                operator: LOOPBACK,
            },
            Some(Provisions {
                driver: Driver::Docker(Docker::provisioning_from(image)),
                runtimes: spawning(&[(RUNTIME, runtime)]),
                max_active_runs: NonZeroUsize::new(2).unwrap(),
            }),
        )
        .await
    }

    async fn booted(environment: Option<Provisions>) -> Self {
        let data_dir = TempDir::new().expect("a temporary data directory");
        Self::boot_against(
            data_dir,
            Listen {
                link: LOOPBACK,
                operator: LOOPBACK,
            },
            environment,
        )
        .await
    }

    async fn boot_against(
        data_dir: TempDir,
        listen: Listen,
        environment: Option<Provisions>,
    ) -> Self {
        let store = Store::open(data_dir.path())
            .await
            .expect("the control plane should boot against a fresh data directory");
        let shutdown = CancellationToken::new();
        let all_in_one = kestrel::role::bind(store.clone(), listen)
            .await
            .expect("the control plane should bind its link");
        let bound = all_in_one.bound();
        let address = bound.link;
        let dispatch = environment.clone().map(|provisions| Dispatch {
            link: match provisions.driver {
                Driver::Docker(_) => format!("http://host.docker.internal:{}", address.port()),
                Driver::LocalExec(_) => format!("http://{address}"),
            },
            driver: provisions.driver,
            runtimes: provisions.runtimes,
            auth: None,
            max_active_runs: provisions.max_active_runs,
            serialized: vec![SERIALIZED.to_owned()],
        });
        let roles = tokio::spawn(all_in_one.run(dispatch, shutdown.clone()));

        Self {
            data_dir,
            store,
            bound,
            environment,
            shutdown,
            roles,
        }
    }

    pub fn data_dir(&self) -> &Path {
        self.data_dir.path()
    }

    pub fn link(&self) -> String {
        format!("http://{}", self.bound.link)
    }

    pub fn operator(&self) -> String {
        format!("http://{}", self.bound.operator)
    }

    pub fn link_from_an_environment(&self) -> String {
        format!("http://host.docker.internal:{}", self.bound.link.port())
    }

    pub async fn declare_organization(&self, name: &str) -> Organization {
        let mut tx = self.store.begin().await.expect("a transaction");
        let organization = tx
            .organizations()
            .declare(name, None)
            .await
            .expect("the organization should declare")
            .record;
        tx.commit().await.expect("the declaration should commit");
        organization
    }

    pub async fn declare_limited_organization(&self, name: &str, maximum: usize) -> Organization {
        let mut tx = self.store.begin().await.expect("a transaction");
        let organization = tx
            .organizations()
            .declare(
                name,
                Some(NonZeroUsize::new(maximum).expect("at least one live instance")),
            )
            .await
            .expect("the organization should declare")
            .record;
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
            .expect("the workspace should declare")
            .record;
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
        agent::declare(&self.store, &organization.name, name, runtime, model)
            .await
            .map(|declared| declared.record)
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

    pub async fn advertised(&self, organization: &Organization, runtime: &str, models: &[&str]) {
        let models: Vec<String> = models.iter().map(|&model| model.to_owned()).collect();
        let mut tx = self.store.begin().await.expect("a transaction");
        tx.agents()
            .record_models_advertised(organization.id, runtime, &models)
            .await
            .expect("the models should record");
        tx.commit().await.expect("the models should commit");
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
                carries,
                connecting: Connecting::Github {
                    repository,
                    api,
                    token: TOKEN,
                    interval,
                    signing_secret: None,
                },
            },
        )
        .await
    }

    pub async fn register_signed_github(
        &self,
        organization: &str,
        name: &str,
        repository: &str,
        api: &str,
        signing_secret: &str,
    ) -> Integration {
        integration::register(
            &self.store,
            Registration {
                organization,
                name,
                carries: &[Direction::Inbound, Direction::Outbound],
                connecting: Connecting::Github {
                    repository,
                    api,
                    token: TOKEN,
                    interval: SignedDuration::from_millis(1),
                    signing_secret: Some(signing_secret),
                },
            },
        )
        .await
        .expect("the integration should register")
    }

    pub async fn register_webhook(
        &self,
        organization: &str,
        name: &str,
        secret: &str,
    ) -> Integration {
        integration::register(
            &self.store,
            Registration {
                organization,
                name,
                carries: &[Direction::Inbound],
                connecting: Connecting::Webhook { secret },
            },
        )
        .await
        .expect("the webhook should register")
    }

    pub async fn integrations(&self, organization: &str) -> Vec<Integration> {
        integration::integrations(&self.store, organization)
            .await
            .expect("the integrations should list")
    }

    pub async fn acknowledge_event_refusal(&self, organization: &str, name: &str) {
        integration::acknowledge_event_refusal(&self.store, organization, name)
            .await
            .expect("the event refusal should be acknowledged");
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
        filter: &str,
        workspace: &str,
        agent: &str,
    ) -> Trigger {
        self.declare_trigger_rendering(
            organization,
            name,
            filter,
            workspace,
            agent,
            &templates(BRIEF, None, None),
        )
        .await
    }

    pub async fn declare_trigger_rendering(
        &self,
        organization: &str,
        name: &str,
        filter: &str,
        workspace: &str,
        agent: &str,
        templates: &Templates,
    ) -> Trigger {
        self.declare_trigger_rendering_with_miss(
            organization,
            name,
            filter,
            workspace,
            agent,
            templates,
            templates
                .correlation
                .is_some()
                .then_some(CorrelationMiss::Open),
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a trigger is what it is declared with"
    )]
    pub async fn declare_trigger_rendering_with_miss(
        &self,
        organization: &str,
        name: &str,
        filter: &str,
        workspace: &str,
        agent: &str,
        templates: &Templates,
        on_miss: Option<CorrelationMiss>,
    ) -> Trigger {
        self.try_declare_trigger_rendering_with_miss(
            organization,
            name,
            filter,
            workspace,
            agent,
            templates,
            on_miss,
        )
        .await
        .expect("the trigger should declare")
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a trigger is what it is declared with"
    )]
    pub async fn try_declare_trigger_rendering_with_miss(
        &self,
        organization: &str,
        name: &str,
        filter: &str,
        workspace: &str,
        agent: &str,
        templates: &Templates,
        on_miss: Option<CorrelationMiss>,
    ) -> anyhow::Result<Trigger> {
        trigger::declare(
            &self.store,
            Declaration {
                organization,
                name,
                fires: &Fires::On(filter.parse().expect("the filter should parse")),
                templates,
                on_miss,
                workspace,
                agent,
                allows: &[],
                profile: None,
            },
        )
        .await
    }

    /// Labelled `ready-for-agent` on the repository, starting its work with `agent` unless a
    /// label chooses one of `allows`.
    pub async fn declare_trigger_allowing(
        &self,
        organization: &str,
        repository: &str,
        agent: &str,
        allows: &[&str],
        correlation: Option<&str>,
    ) -> Trigger {
        trigger::declare(
            &self.store,
            Declaration {
                organization,
                name: "ready",
                fires: &Fires::On(
                    labelled_on(repository, "ready-for-agent")
                        .parse()
                        .expect("the filter should parse"),
                ),
                templates: &templates(BRIEF, None, correlation),
                on_miss: correlation.map(|_| CorrelationMiss::Open),
                workspace: "kestrel",
                agent,
                allows: &allows
                    .iter()
                    .map(|&name| name.to_owned())
                    .collect::<Vec<_>>(),
                profile: None,
            },
        )
        .await
        .expect("the trigger should declare")
    }

    pub async fn apply_triggers(&self, organization: &str, file: &str) -> Applied {
        trigger::apply::apply(
            &self.store,
            organization,
            &trigger::apply::parse(file).expect("the declaration file should parse"),
            false,
        )
        .await
        .expect("the declaration file should apply")
    }

    pub async fn test_trigger(
        &self,
        organization: &str,
        name: &str,
        event: EventRecordId,
    ) -> Tested {
        self.try_test_trigger(organization, name, event)
            .await
            .expect("the trigger should test")
    }

    pub async fn try_test_trigger(
        &self,
        organization: &str,
        name: &str,
        event: EventRecordId,
    ) -> anyhow::Result<Tested> {
        trigger::test(&self.store, organization, name, Some(event), None).await
    }

    pub async fn dispatch(
        &self,
        organization: &str,
        name: &str,
        issue: i64,
        asked: trigger::Asked<'_>,
    ) -> anyhow::Result<trigger::Fired> {
        trigger::dispatch(
            &self.store,
            &kestrel::integration::github::Github::dialling_out()?,
            trigger::Dispatch {
                organization,
                trigger: name,
                integration: "github",
                issue,
                asked,
            },
        )
        .await
    }

    pub async fn try_declare_scheduled_trigger(
        &self,
        organization: &str,
        name: &str,
        schedule: Schedule,
        templates: &Templates,
    ) -> anyhow::Result<Trigger> {
        trigger::declare(
            &self.store,
            Declaration {
                organization,
                name,
                fires: &Fires::Scheduled(schedule),
                templates,
                on_miss: templates
                    .correlation
                    .is_some()
                    .then_some(CorrelationMiss::Open),
                workspace: "kestrel",
                agent: "builder",
                allows: &[],
                profile: None,
            },
        )
        .await
    }

    pub async fn test_scheduled_trigger(&self, organization: &str, name: &str) -> Tested {
        self.try_test_trigger_naming_no_event(organization, name)
            .await
            .expect("the trigger should test against its next elapsing")
    }

    pub async fn try_test_trigger_naming_no_event(
        &self,
        organization: &str,
        name: &str,
    ) -> anyhow::Result<Tested> {
        trigger::test(&self.store, organization, name, None, None).await
    }

    /// Stands in for the wheel reaching `at`, which a test cannot wait for.
    pub async fn elapse(&self, at: Timestamp) -> Vec<Occurrence> {
        trigger::elapse(&self.store, at)
            .await
            .expect("the due schedules should elapse")
    }

    pub async fn test_declared_trigger(
        &self,
        organization: &str,
        file: &str,
        name: &str,
        event: EventRecordId,
    ) -> Tested {
        let declarations = trigger::apply::parse(file).expect("the declaration file should parse");
        let declared = declarations
            .iter()
            .find(|declared| declared.name == name)
            .expect("the declaration file should declare the trigger");

        trigger::test_declared(&self.store, organization, declared, Some(event), None)
            .await
            .expect("the declared trigger should test")
    }

    pub async fn firings(&self, event: EventRecordId) -> Vec<kestrel::domain::Firing> {
        trigger::firings(&self.store, event)
            .await
            .expect("the firings should read")
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

    pub async fn declare_profile(
        &self,
        organization: &str,
        name: &str,
        owner: &str,
    ) -> anyhow::Result<SubscriptionProfile> {
        profile::declare(&self.store, organization, name, owner)
            .await
            .map(|declared| declared.record)
    }

    pub async fn hold_in_profile(
        &self,
        organization: &str,
        name: &str,
        entry: &profile::Entry,
        login: &str,
    ) {
        profile::hold(&self.store, organization, name, entry, login)
            .await
            .expect("the login should be held");
    }

    pub async fn profiles(
        &self,
        organization: &str,
    ) -> Vec<(SubscriptionProfile, Vec<profile::Held>)> {
        profile::profiles(&self.store, organization)
            .await
            .expect("the profiles should list")
    }

    /// What the next Run spawned with the profile would be handed.
    pub async fn profile_contents(&self, profile: &SubscriptionProfile) -> Contents {
        profile::contents(&self.store, profile)
            .await
            .expect("the profile should open")
    }

    pub async fn open_session_with(
        &self,
        organization: &str,
        workspace: &str,
        agent: &str,
        profile: &str,
    ) -> Session {
        session::open(
            &self.store,
            organization,
            workspace,
            agent,
            Some(profile),
            None,
            None,
        )
        .await
        .expect("the session should open")
    }

    pub async fn open_session(&self, organization: &str, workspace: &str, agent: &str) -> Session {
        self.try_open_session(organization, workspace, agent, None)
            .await
            .expect("the session should open")
    }

    pub async fn open_session_on(
        &self,
        organization: &str,
        workspace: &str,
        agent: &str,
        branch: &str,
    ) -> Session {
        session::open(
            &self.store,
            organization,
            workspace,
            agent,
            None,
            Some(branch),
            None,
        )
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
        let continues = continues.map(|sealed| sealed.to_string());
        session::open(
            &self.store,
            organization,
            workspace,
            agent,
            None,
            None,
            continues.as_deref(),
        )
        .await
    }

    pub async fn seal_session(&self, id: SessionId) -> Session {
        self.try_seal_session(id)
            .await
            .expect("the session should seal")
    }

    pub async fn try_seal_session(&self, id: SessionId) -> anyhow::Result<Session> {
        session::seal(&self.store, id).await
    }

    pub async fn held_instances(&self, organization: &str) -> Vec<instance::Held> {
        instance::held(&self.store, organization)
            .await
            .expect("the held instances should read")
    }

    pub async fn release_instance(&self, session: SessionId) -> String {
        self.try_release_instance(session)
            .await
            .expect("the instance should release")
    }

    pub async fn try_release_instance(&self, session: SessionId) -> anyhow::Result<String> {
        instance::release(&self.store, session, "operator").await
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
        let session = tx.sessions().get(run.session).await?;
        tx.log()
            .append(
                &session,
                Entry::Said {
                    participant: session.agent.name.clone(),
                    message: message.to_owned(),
                },
            )
            .await?;
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
        work::enqueue(&self.store, session, None).await
    }

    pub async fn enqueue_run_naming(&self, session: SessionId, model: Option<&str>) -> Run {
        self.try_enqueue_run_naming(session, model)
            .await
            .expect("the run should enqueue")
    }

    pub async fn try_enqueue_run_naming(
        &self,
        session: SessionId,
        model: Option<&str>,
    ) -> anyhow::Result<Run> {
        work::enqueue(&self.store, session, model).await
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
        work::claim(&self.store, &[SERIALIZED.to_owned()])
            .await
            .expect("the claim should ask")
    }

    pub async fn occupy_run(&self) -> Option<Claimed> {
        match work::occupy(&self.store, 2, &[SERIALIZED.to_owned()])
            .await
            .expect("the occupancy should ask")
        {
            Some(work::Occupied::Claimed(claimed)) => Some(claimed),
            Some(work::Occupied::Resumed(_)) => panic!("no run should resume"),
            None => None,
        }
    }

    /// Prompts a Run between turns with what is held for it, the way the work role's sweep does.
    pub async fn prompt_waiting(&self) {
        work::occupy(&self.store, 1, &[SERIALIZED.to_owned()])
            .await
            .expect("the occupancy should ask");
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

    pub async fn turns(&self, run: RunId) -> Vec<Turn> {
        work::turns(&self.store, run)
            .await
            .expect("the run's turns should read")
    }

    pub async fn stop_run(&self, run: RunId) -> Exit {
        self.try_stop_run(run).await.expect("the run should stop")
    }

    pub async fn try_stop_run(&self, run: RunId) -> anyhow::Result<Exit> {
        work::stop(&self.store, run).await
    }

    pub async fn answered(&self, run: RunId, count: usize) -> Run {
        self.answered_within(run, count, PATIENCE).await
    }

    /// Once `count` of the Run's turns are answered, or once it has ended short of them.
    pub async fn answered_within(
        &self,
        run: RunId,
        count: usize,
        patience: std::time::Duration,
    ) -> Run {
        let deadline = tokio::time::Instant::now() + patience;

        loop {
            let answered = self
                .turns(run)
                .await
                .iter()
                .filter(|turn| turn.answered_at.is_some())
                .count();
            let run = self.run(run).await;
            if answered >= count || run.state == RunState::Ended {
                return run;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the run {} is {} with {answered} of {count} turns answered",
                run.id,
                run.state
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// A Run whose first turn is over has ended either way: by that turn, or stopped after it
    /// the way an operator would, because answering never ends one (ADR-0024).
    pub async fn after_one_turn(&self, run: RunId) -> Run {
        self.after_one_turn_within(run, PATIENCE).await
    }

    pub async fn after_one_turn_within(&self, run: RunId, patience: std::time::Duration) -> Run {
        let answered = self.answered_within(run, 1, patience).await;
        if answered.state != RunState::Ended {
            self.try_stop_run(run)
                .await
                .expect("a run between turns should stop");
        }

        self.run(run).await
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

    pub async fn supervised(&self, run: &Run, supervisor: &str) {
        work::supervised(&self.store, run, supervisor)
            .await
            .expect("the supervisor should be recorded");
    }

    pub async fn executes_on(&self, run: &Run, instance: &str) {
        work::executes_on(&self.store, run, instance)
            .await
            .expect("the instance should be recorded");
    }

    pub async fn report_checkout(&self, run: &Run, repositories: Vec<instance::Observed>) {
        work::report(
            &self.store,
            run,
            work::Reported {
                seq: Some(1),
                report: work::Report::Checkout { repositories },
            },
        )
        .await
        .expect("the checkout should be reported");
    }

    pub async fn instances_to_archive(&self) -> Vec<String> {
        instance::to_archive(&self.store)
            .await
            .expect("the instances to archive should read")
    }

    pub async fn instance_archived(&self, instance: &str) {
        instance::archived(&self.store, instance)
            .await
            .expect("the instance should be recorded archived");
    }

    pub async fn supervisors_to_stop(&self) -> Vec<(Run, String)> {
        work::supervisors_to_stop(&self.store)
            .await
            .expect("ended runs' supervisors should read")
    }

    pub async fn supervisor_gone(&self, run: &Run) {
        work::supervisor_gone(&self.store, run)
            .await
            .expect("the supervisor should be gone");
    }

    pub async fn instance(&self, session: SessionId) -> Option<String> {
        work::instance(&self.store, session)
            .await
            .expect("the session's instance should read")
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

    pub async fn start(&self, run: &Run) {
        self.try_start(run).await.expect("the run should start");
    }

    pub async fn try_start(&self, run: &Run) -> anyhow::Result<link::SentInstruction> {
        link::start(&self.store, run).await
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
            bound: self.bound,
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
        destroy_instances(self.data_dir.path(), self.environment.as_ref()).await;
        drop(self.store);

        Stopped {
            data_dir: self.data_dir,
            bound: self.bound,
            environment: self.environment,
        }
    }
}

/// An Instance outlives every Run on it and nothing here seals a Session into releasing one,
/// so a test's Instances go with the test.
async fn destroy_instances(data_dir: &Path, provisions: Option<&Provisions>) {
    let Some(provisions) = provisions else {
        return;
    };
    let database = data_dir.join("kestrel.db");
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", database.display()))
        .await
        .expect("the database should open");
    let instances: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT instance FROM run WHERE instance IS NOT NULL")
            .fetch_all(&pool)
            .await
            .expect("the instances should read");
    pool.close().await;

    for instance in instances {
        let _ = provisions.driver.destroy_named(&instance);
    }
}

impl Stopped {
    pub async fn restart(self) -> Harness {
        Harness::boot_against(self.data_dir, self.bound, self.environment).await
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
