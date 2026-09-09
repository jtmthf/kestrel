use std::num::NonZeroUsize;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::cli::Role;
use crate::compute::{Driver, Environment, Exited};
use crate::domain::{Exit, Run, Session, Workspace};
use crate::link::{self, Instruction};
use crate::provider;
use crate::session;
use crate::store::Store;
use crate::timer;
use crate::work::{self, Claimed};

/// Nothing subscribes to `Fanout` at 0.1 (ADR-0005), so a queued Run is found by asking
/// `Store` again rather than by being told.
const POLL: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub struct Dispatch {
    pub link: String,
    pub driver: Driver,
    pub runtime: String,
    pub auth: Option<String>,
    pub max_active_runs: NonZeroUsize,
}

impl Dispatch {
    fn logs_the_agent_in(&self) -> bool {
        self.auth
            .as_deref()
            .is_some_and(|method| !method.is_empty())
    }
}

/// What ended the attending, rather than how the Run went.
enum Ended {
    Environment(Exited),
    TheRun(Exit),
    ControlPlane,
}

/// The wheel keeps time whether or not this role has anywhere to dispatch a Run, because a
/// lease left unswept wedges a Session no matter who was going to execute it.
pub async fn run(
    store: Store,
    dispatch: Option<Dispatch>,
    shutdown: CancellationToken,
) -> Result<()> {
    info!(role = %Role::Work, "role started");

    tokio::try_join!(
        timer::sweeping(&store, &shutdown),
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
        reap(store, &dispatch.driver).await?;
        if active.len() < dispatch.max_active_runs.get()
            && let Some(claimed) = work::claim(store).await?
        {
            let store = store.clone();
            let dispatch = dispatch.clone();
            let shutdown = shutdown.clone();
            active.spawn(async move { execute(&store, &dispatch, claimed, &shutdown).await });
            continue;
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

    let mut environment = match dispatch.driver.provision(
        run.id,
        &[
            ("KESTREL_LINK", dispatch.link.as_str()),
            ("KESTREL_RUN", &run.id.to_string()),
            ("KESTREL_RUN_CREDENTIAL", credential.as_str()),
            ("KESTREL_AGENT_RUNTIME", dispatch.runtime.as_str()),
            (
                "KESTREL_AGENT_AUTH",
                dispatch.auth.as_deref().unwrap_or_default(),
            ),
            (
                "KESTREL_AGENT_MODEL",
                session.agent.model.as_deref().unwrap_or_default(),
            ),
        ],
    ) {
        Ok(environment) => environment,
        Err(error) => {
            work::fail(
                store,
                &run,
                &format!("the environment could not be provisioned: {error}"),
            )
            .await?;
            return Ok(());
        }
    };
    work::environment_present(store, &run, environment.name()).await?;

    let exit = match check_out(&session.workspace, &mut environment).await {
        Ok(()) => start(store, &run, environment, shutdown).await?,
        Err(error) => {
            let exit = work::fail(store, &run, &error.to_string()).await?;
            if destroy(&run, environment) {
                work::environment_gone(store, &run).await?;
            }
            exit
        }
    };
    info!(run = %run.id, %exit, "a run ended");

    Ok(())
}

async fn reap(store: &Store, driver: &Driver) -> Result<()> {
    for (run, environment) in work::environments_to_reap(store).await? {
        match driver.destroy_named(run.id, &environment) {
            Ok(()) => {
                work::environment_gone(store, &run).await?;
            }
            Err(error) => {
                warn!(run = %run.id, %error, "an ended run's environment resisted being reaped");
            }
        }
    }

    Ok(())
}

/// An Agent Runtime reaches a model with a Provider Credential its Organization holds, or by
/// an ACP login kestrel was configured with. A Run with neither fails here rather than inside
/// an Environment provisioned to find that out.
async fn a_way_to_reach_a_model(
    store: &Store,
    dispatch: &Dispatch,
    session: &Session,
) -> Result<()> {
    if dispatch.logs_the_agent_in() || provider::holds_any(store, session.organization.id).await? {
        return Ok(());
    }

    bail!(
        "the organization {} holds no provider credential, and this run's agent runtime was \
         given no other way to reach a model",
        session.organization.name
    )
}

async fn start(
    store: &Store,
    run: &Run,
    mut environment: Environment,
    shutdown: &CancellationToken,
) -> Result<Exit> {
    let name = environment.name().to_owned();
    work::provisioned(store, run, &name).await?;
    link::instruct(store, run, Instruction::Start).await?;
    info!(run = %run.id, environment = name, "a run reached an environment");

    let ended = attend(store, run, &mut environment, shutdown).await;
    let was_already_gone = matches!(ended, Ok(Ended::Environment(_)));

    let exit = match ended? {
        Ended::TheRun(exit) => exit,
        Ended::Environment(exited) => {
            let unreported =
                format!("the environment exited {exited} without reporting how the run went");
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

    let destroyed = destroy(run, environment);
    if was_already_gone || destroyed {
        work::environment_gone(store, run).await?;
    }

    Ok(exit)
}

fn destroy(run: &Run, environment: Environment) -> bool {
    match environment.destroy() {
        Ok(()) => true,
        Err(error) => {
            warn!(run = %run.id, %error, "an environment resisted being destroyed");
            false
        }
    }
}

/// Before the Run is told to start, so nothing an agent reaches for is still arriving.
async fn check_out(workspace: &Workspace, environment: &mut Environment) -> Result<()> {
    for repository in &workspace.repositories {
        let cloning = environment
            .exec(&["git", "clone", "--branch", &workspace.branch, repository])
            .with_context(|| format!("{repository} could not be cloned into the environment"))?;
        let cloned = tokio::task::spawn_blocking(move || cloning.finish()).await??;

        if !cloned.exited.success() {
            bail!(
                "{repository} could not be cloned into the environment: {}",
                cloned.err
            );
        }
    }

    Ok(())
}

/// The Environment reports its own outcome over the link, so what this waits for is the
/// Environment being gone. It stops for a Run that ended some other way too — a lease the
/// Environment stopped holding out — because an Environment that outlives its Run would
/// otherwise hold this role's one dispatch forever.
async fn attend(
    store: &Store,
    run: &Run,
    environment: &mut Environment,
    shutdown: &CancellationToken,
) -> Result<Ended> {
    loop {
        match environment.status() {
            Ok(Some(exited)) => return Ok(Ended::Environment(exited)),
            Ok(None) => {}
            // A daemon that cannot answer is not an Environment that is gone. The Run's lease
            // ends it if this never clears.
            Err(error) => {
                warn!(run = %run.id, %error, "an environment could not be asked how it is")
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
