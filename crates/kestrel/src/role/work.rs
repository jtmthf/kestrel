use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::cli::Role;
use crate::compute::{Environment, LocalExec};
use crate::domain::Run;
use crate::link::{self, Instruction};
use crate::store::Store;
use crate::work::{self, Claimed};

/// Nothing subscribes to `Fanout` at 0.1 (ADR-0005), so a queued Run is found by asking
/// `Store` again rather than by being told.
const POLL: Duration = Duration::from_millis(100);
const HEARTBEAT: Duration = Duration::from_secs(1);

pub struct Dispatch {
    pub link: String,
    pub supervisor: PathBuf,
    pub runtime: String,
}

enum Ended {
    Environment(ExitStatus),
    ControlPlaneStopped,
}

/// A work role with nowhere to run a Run claims none: claiming one it cannot dispatch would
/// spend the Run's one dispatch on nothing.
pub async fn run(
    store: Store,
    dispatch: Option<Dispatch>,
    shutdown: CancellationToken,
) -> Result<()> {
    info!(role = %Role::Work, "role started");

    match dispatch {
        Some(dispatch) => dispatching(&store, &dispatch, &shutdown).await?,
        None => shutdown.cancelled().await,
    }

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
    let mut environment = match LocalExec.provision(
        &dispatch.supervisor,
        &[],
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

    let name = environment.name();
    work::provisioned(store, &run, &name).await?;
    link::instruct(store, &run, Instruction::Start).await?;
    info!(run = %run.id, environment = name, "a run reached an environment");

    let ended = attend(store, &run, &mut environment, shutdown).await;
    if let Err(error) = LocalExec.destroy(environment) {
        warn!(run = %run.id, %error, "an environment resisted being destroyed");
    }

    let unreported = match ended? {
        Ended::Environment(status) => {
            format!("the environment exited {status} without reporting how the run went")
        }
        Ended::ControlPlaneStopped => {
            "the control plane stopped while this run was in flight".to_owned()
        }
    };
    let exit = work::fail(store, &run, &unreported).await?;
    info!(run = %run.id, %exit, "a run ended");

    Ok(())
}

/// The Environment reports its own outcome over the link, so what this waits for is the
/// Environment being gone rather than the Run's exit status.
async fn attend(
    store: &Store,
    run: &Run,
    environment: &mut Environment,
    shutdown: &CancellationToken,
) -> Result<Ended> {
    let mut heartbeat = tokio::time::interval(HEARTBEAT);

    loop {
        if let Some(status) = environment.status()? {
            return Ok(Ended::Environment(status));
        }

        tokio::select! {
            _ = heartbeat.tick() => work::heartbeat(store, run).await?,
            () = tokio::time::sleep(POLL) => {}
            () = shutdown.cancelled() => return Ok(Ended::ControlPlaneStopped),
        }
    }
}
