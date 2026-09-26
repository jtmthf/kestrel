use std::io::{BufRead as _, BufReader, Read};
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use jiff::{SignedDuration, Timestamp};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::cli::Role;
use crate::compute::{Driver, Exited, Instance, Supervisor};
use crate::domain::{Exit, Run, RunId, Session};
use crate::instance;
use crate::link;
use crate::profile;
use crate::provider;
use crate::session;
use crate::store::Store;
use crate::timer;
use crate::work::{self, Claimed, Occupied};

/// Nothing subscribes to `Fanout` at 0.1 (ADR-0005), so a queued Run is found by asking
/// `Store` again rather than by being told.
const POLL: Duration = Duration::from_millis(100);
const LEAVING: SignedDuration = SignedDuration::from_secs(3);

#[derive(Clone)]
pub struct Dispatch {
    pub link: String,
    pub driver: Driver,
    pub harnesses: Vec<HarnessCommand>,
    pub auth: Option<String>,
    pub max_active_runs: NonZeroUsize,
    pub serialized: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCommand {
    pub name: String,
    pub command: String,
}

impl FromStr for HarnessCommand {
    type Err = anyhow::Error;

    fn from_str(given: &str) -> Result<Self> {
        match given.split_once('=') {
            Some((name, command)) if !name.is_empty() && !command.trim().is_empty() => Ok(Self {
                name: name.to_owned(),
                command: command.to_owned(),
            }),
            _ => bail!("{given} is not NAME=COMMAND"),
        }
    }
}

impl Dispatch {
    fn spawns(&self, harness: &str) -> Result<&str> {
        self.harnesses
            .iter()
            .find(|spawned| spawned.name == harness)
            .map(|spawned| spawned.command.as_str())
            .with_context(|| format!("this work role spawns no harness named {harness}"))
    }

    fn logs_the_agent_in(&self) -> bool {
        self.auth
            .as_deref()
            .is_some_and(|method| !method.is_empty())
    }
}

/// What ended the attending, rather than how the Run went.
enum Ended {
    Supervisor(Exited),
    TheRun(Exit),
    ControlPlane,
}

/// The wheel keeps time whether or not this role has anywhere to dispatch a Run, because a
/// lease left unswept wedges a Session no matter who was going to execute it.
pub async fn run(
    store: Store,
    dispatch: Option<Dispatch>,
    wake: timer::Wake,
    shutdown: CancellationToken,
) -> Result<()> {
    info!(role = %Role::Work, "role started");

    tokio::try_join!(
        timer::sweeping(&store, &wake, &shutdown),
        // A work role with nowhere to run a Run claims none: claiming one it cannot dispatch
        // would spend the Run's one dispatch on nothing.
        async {
            match &dispatch {
                Some(dispatch) => dispatching(&store, dispatch, &shutdown).await,
                None => {
                    shutdown.cancelled().await;
                    Ok(())
                }
            }
        },
    )?;

    info!(role = %Role::Work, "role stopped");
    Ok(())
}

async fn dispatching(
    store: &Store,
    dispatch: &Dispatch,
    shutdown: &CancellationToken,
) -> Result<()> {
    let mut active = JoinSet::new();

    while !shutdown.is_cancelled() {
        stop_left_behind(store, &dispatch.driver).await?;
        archive(store, &dispatch.driver).await?;
        match work::occupy(store, dispatch.max_active_runs.get(), &dispatch.serialized).await? {
            Some(Occupied::Claimed(claimed)) => {
                let store = store.clone();
                let dispatch = dispatch.clone();
                let shutdown = shutdown.clone();
                active.spawn(async move { execute(&store, &dispatch, claimed, &shutdown).await });
                continue;
            }
            Some(Occupied::Resumed(run)) => {
                info!(run = %run.id, "a waiting run was prompted with what was held for it");
                continue;
            }
            None => {}
        }

        tokio::select! {
            finished = active.join_next(), if !active.is_empty() => {
                finished.expect("an active run")
                    .context("a run's execution task failed")??;
            }
            () = tokio::time::sleep(POLL) => {}
            () = shutdown.cancelled() => {}
        }
    }

    while let Some(finished) = active.join_next().await {
        finished.context("a run's execution task failed")??;
    }

    Ok(())
}

async fn execute(
    store: &Store,
    dispatch: &Dispatch,
    Claimed { run, credential }: Claimed,
    shutdown: &CancellationToken,
) -> Result<()> {
    let session = match session::show(store, run.session).await {
        Ok(session) => session,
        Err(error) => {
            work::fail(
                store,
                &run,
                &format!("the run's session could not be read: {error}"),
            )
            .await?;
            return Ok(());
        }
    };

    if let Err(error) = a_way_to_reach_a_model(store, dispatch, &session).await {
        work::fail(store, &run, &error.to_string()).await?;
        return Ok(());
    }
    let command = match dispatch.spawns(&session.agent.harness) {
        Ok(command) => command,
        Err(error) => {
            work::fail(store, &run, &error.to_string()).await?;
            return Ok(());
        }
    };

    let Some(mut instance) = instance(store, dispatch, &run, &session).await? else {
        return Ok(());
    };
    work::executes_on(store, &run, instance.name()).await?;

    // Committed before the supervisor is spawned, so one that outlives this process fetches its
    // Start on reconnect rather than holding the lease out forever on a Run that cannot begin.
    if let Err(error) = link::start(store, &run).await {
        work::fail(
            store,
            &run,
            &format!("the run could not be started: {error}"),
        )
        .await?;
        return Ok(());
    }

    let mut supervisor = match instance.supervise(&[
        ("KESTREL_LINK", dispatch.link.as_str()),
        ("KESTREL_RUN", &run.id.to_string()),
        ("KESTREL_RUN_CREDENTIAL", credential.as_str()),
        ("KESTREL_HARNESS_COMMAND", command),
        (
            "KESTREL_AGENT_AUTH",
            dispatch.auth.as_deref().unwrap_or_default(),
        ),
        (
            "KESTREL_AGENT_MODEL",
            run.model
                .as_deref()
                .or(session.agent.model.as_deref())
                .unwrap_or_default(),
        ),
    ]) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            work::fail(
                store,
                &run,
                &format!(
                    "the supervisor could not be started on the instance {}: {error}",
                    instance.name()
                ),
            )
            .await?;
            return Ok(());
        }
    };
    work::supervised(store, &run, supervisor.name()).await?;
    if let Some(out) = supervisor.take_stdout() {
        relay(run.id, out);
    }
    if let Some(err) = supervisor.take_stderr() {
        relay(run.id, err);
    }

    let exit = start(store, &run, supervisor, shutdown).await?;
    info!(run = %run.id, %exit, "a run ended");

    Ok(())
}

/// The Session's own Instance, or a fresh one for a Session that has none. `None` once the Run
/// has been ended for want of one.
async fn instance(
    store: &Store,
    dispatch: &Dispatch,
    run: &Run,
    session: &Session,
) -> Result<Option<Instance>> {
    let Some(kept) = work::instance(store, session.id).await? else {
        return match dispatch.driver.provision(run.id) {
            Ok(instance) => Ok(Some(instance)),
            Err(error) => {
                work::fail(
                    store,
                    run,
                    &format!("the instance could not be provisioned: {error}"),
                )
                .await?;
                Ok(None)
            }
        };
    };

    match dispatch.driver.resume(&kept) {
        Ok(Some(instance)) => Ok(Some(instance)),
        Ok(None) => {
            let because = format!(
                "the instance {kept} this session's work was on is gone, and whatever it held \
                 that was never pushed went with it; the session's next run starts on a fresh \
                 instance from the branch {} as the remote has it",
                session.checkout.branch
            );
            work::instance_lost(store, run, &because).await?;
            Ok(None)
        }
        // Not forgotten: a daemon that cannot answer has not lost what the Instance holds.
        Err(error) => {
            work::fail(
                store,
                run,
                &format!("the instance {kept} could not be resumed: {error}"),
            )
            .await?;
            Ok(None)
        }
    }
}

/// A supervisor blocks once a pipe nobody reads is full, so what it says is read as it says it.
fn relay(run: RunId, said: impl Read + Send + 'static) {
    std::thread::spawn(move || {
        for line in BufReader::new(said).lines().map_while(Result::ok) {
            info!(run = %run, "{line}");
        }
    });
}

async fn stop_left_behind(store: &Store, driver: &Driver) -> Result<()> {
    for (run, supervisor) in work::supervisors_to_stop(store).await? {
        if run
            .ended_at
            .is_some_and(|ended| Timestamp::now().duration_since(ended) < LEAVING)
        {
            continue;
        }
        match driver.stop_named(&supervisor) {
            Ok(()) => {
                work::supervisor_gone(store, &run).await?;
            }
            Err(error) => {
                warn!(run = %run.id, %error, "an ended run's supervisor resisted being stopped");
            }
        }
    }

    Ok(())
}

async fn archive(store: &Store, driver: &Driver) -> Result<()> {
    for instance in instance::to_archive(store).await? {
        match driver.destroy_named(&instance) {
            Ok(()) => {
                instance::archived(store, &instance).await?;
                info!(instance, "an instance was archived");
            }
            Err(error) => warn!(instance, %error, "an instance resisted being archived"),
        }
    }

    Ok(())
}

/// A Harness reaches a model with the Session's Subscription Profile, a Provider
/// Credential its Organization holds, or an ACP login kestrel was configured with. A Run with
/// none of them fails here rather than inside an Instance provisioned to find that out.
async fn a_way_to_reach_a_model(
    store: &Store,
    dispatch: &Dispatch,
    session: &Session,
) -> Result<()> {
    if let Some(named) = &session.profile {
        if profile::holds_anything(store, named).await? {
            return Ok(());
        }
        bail!(
            "the subscription profile {} this session names holds no login",
            named.name
        );
    }
    if dispatch.logs_the_agent_in() || provider::holds_any(store, session.organization.id).await? {
        return Ok(());
    }

    bail!(
        "the organization {} holds no provider credential, and this run's harness was \
         given no other way to reach a model",
        session.organization.name
    )
}

async fn start(
    store: &Store,
    run: &Run,
    mut supervisor: Supervisor,
    shutdown: &CancellationToken,
) -> Result<Exit> {
    info!(run = %run.id, supervisor = supervisor.name(), "a run's supervisor started");

    let ended = attend(store, run, &mut supervisor, shutdown).await;

    let exit = match ended? {
        Ended::TheRun(exit) => {
            left_the_link(&mut supervisor).await;
            exit
        }
        Ended::Supervisor(exited) => {
            let unreported =
                format!("the supervisor exited {exited} without reporting how the run went");
            work::fail(store, run, &unreported).await?
        }
        Ended::ControlPlane => {
            work::fail(
                store,
                run,
                "the control plane stopped while this run was in flight",
            )
            .await?
        }
    };

    match supervisor.stop() {
        Ok(()) => {
            work::supervisor_gone(store, run).await?;
        }
        Err(error) => warn!(run = %run.id, %error, "a supervisor resisted being stopped"),
    }

    Ok(exit)
}

/// A Run stopped or sealed tells its supervisor to leave, and one that does closes its agent
/// conversation on the way out; killed first, it would leave the agent's own process group
/// running.
async fn left_the_link(supervisor: &mut Supervisor) {
    let deadline = tokio::time::Instant::now() + LEAVING.unsigned_abs();

    while tokio::time::Instant::now() < deadline {
        if !matches!(supervisor.status(), Ok(None)) {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The supervisor reports its own outcome over the link, so what this waits for is the
/// supervisor being gone. It stops for a Run that ended some other way too — a lease the
/// supervisor stopped holding out — because a supervisor that outlives its Run would otherwise
/// hold this role's one dispatch forever.
async fn attend(
    store: &Store,
    run: &Run,
    supervisor: &mut Supervisor,
    shutdown: &CancellationToken,
) -> Result<Ended> {
    loop {
        match supervisor.status() {
            Ok(Some(exited)) => return Ok(Ended::Supervisor(exited)),
            Ok(None) => {}
            // A daemon that cannot answer is not a supervisor that is gone. The Run's lease
            // ends it if this never clears.
            Err(error) => {
                warn!(run = %run.id, %error, "a supervisor could not be asked how it is")
            }
        }
        if let Some(exit) = work::run(store, run.id).await?.exit {
            return Ok(Ended::TheRun(exit));
        }

        tokio::select! {
            () = tokio::time::sleep(POLL) => {}
            () = shutdown.cancelled() => return Ok(Ended::ControlPlane),
        }
    }
}
