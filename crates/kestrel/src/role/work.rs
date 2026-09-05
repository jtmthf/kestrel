use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::cli::Role;
use crate::compute::{Driver, Environment, Exited};
use crate::domain::{Exit, Run};
use crate::link::{self, Instruction};
use crate::session;
use crate::store::Store;
use crate::timer;
use crate::work::{self, Claimed};

/// Nothing subscribes to `Fanout` at 0.1 (ADR-0005), so a queued Run is found by asking
/// `Store` again rather than by being told.
const POLL: Duration = Duration::from_millis(100);

pub struct Dispatch {
    pub link: String,
    pub driver: Driver,
    pub runtime: String,
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
    while !shutdown.is_cancelled() {
        match work::claim(store).await? {
            Some(claimed) => execute(store, dispatch, claimed, shutdown).await?,
            None => {
                tokio::select! {
                    () = tokio::time::sleep(POLL) => {}
                    () = shutdown.cancelled() => {}
                }
            }
        }
    }

    Ok(())
}

async fn execute(
    store: &Store,
    dispatch: &Dispatch,
    Claimed { run, credential }: Claimed,
    shutdown: &CancellationToken,
) -> Result<()> {
    let mut environment = match dispatch.driver.provision(
        run.id,
        &[
            ("KESTREL_LINK", dispatch.link.as_str()),
            ("KESTREL_RUN", &run.id.to_string()),
            ("KESTREL_RUN_CREDENTIAL", credential.as_str()),
            ("KESTREL_AGENT_RUNTIME", dispatch.runtime.as_str()),
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

    let exit = match check_out(store, &run, &mut environment).await {
        Ok(()) => start(store, &run, environment, shutdown).await?,
        Err(error) => {
            destroy(&run, environment);
            work::fail(store, &run, &error.to_string()).await?
        }
    };
    info!(run = %run.id, %exit, "a run ended");

    Ok(())
}

async fn start(
    store: &Store,
    run: &Run,
    mut environment: Environment,
    shutdown: &CancellationToken,
) -> Result<Exit> {
    // Recorded once the Workspace is in it and it is about to be started, so a Run that names
    // an Environment is a Run something is working on.
    let name = environment.name().to_owned();
    work::provisioned(store, run, &name).await?;
    link::instruct(store, run, Instruction::Start).await?;
    info!(run = %run.id, environment = name, "a run reached an environment");

    let ended = attend(store, run, &mut environment, shutdown).await;
    destroy(run, environment);

    Ok(match ended? {
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
    })
}

fn destroy(run: &Run, environment: Environment) {
    if let Err(error) = environment.destroy() {
        warn!(run = %run.id, %error, "an environment resisted being destroyed");
    }
}

/// Before the Run is told to start, so nothing an agent reaches for is still arriving.
async fn check_out(store: &Store, run: &Run, environment: &mut Environment) -> Result<()> {
    let workspace = session::show(store, run.session).await?.workspace;

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
