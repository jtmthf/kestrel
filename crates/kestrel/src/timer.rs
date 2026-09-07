//! The in-process wheel (ADR-0005). The schedule is never in process memory alone, so a
//! control plane that restarts finds every due time set before it existed and fires it.

use std::time::Duration;

use anyhow::Result;
use jiff::Timestamp;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::domain::{Exit, RunId};
use crate::integration::github::Github;
use crate::integration::{self, Polled};
use crate::store::Store;
use crate::trigger;
use crate::work;

const SWEEP: Duration = Duration::from_millis(500);

pub async fn sweeping(store: &Store, shutdown: &CancellationToken) -> Result<()> {
    // Beside the lease sweep rather than in it: a poll waits on GitHub, and a lease left
    // unswept for the length of an HTTP request is a Session wedged for that long.
    tokio::try_join!(
        sweeping_leases(store, shutdown),
        polling(store, shutdown),
        firing(store, shutdown)
    )?;

    Ok(())
}

async fn sweeping_leases(store: &Store, shutdown: &CancellationToken) -> Result<()> {
    while !shutdown.is_cancelled() {
        // The database being busy is not a reason to stop keeping time: the same due times
        // are still there to be found on the next sweep.
        match sweep(store).await {
            Ok(expired) => {
                for (run, exit) in expired {
                    info!(%run, %exit, "a lease expired");
                }
            }
            Err(error) => warn!(%error, "a sweep found nothing it could do"),
        }

        tick(shutdown).await;
    }

    Ok(())
}

async fn polling(store: &Store, shutdown: &CancellationToken) -> Result<()> {
    let github = Github::dialling_out()?;

    while !shutdown.is_cancelled() {
        match poll(store, &github).await {
            Ok(()) => {}
            Err(error) => warn!(%error, "a poll found nothing it could do"),
        }

        tick(shutdown).await;
    }

    Ok(())
}

/// Matching is its own sweep rather than the tail of a poll, so a Trigger declared after an
/// Event was recorded still fires for it, and a control plane that stopped between recording
/// an Event and firing for it finds it on the way back up.
async fn firing(store: &Store, shutdown: &CancellationToken) -> Result<()> {
    while !shutdown.is_cancelled() {
        match trigger::fire(store).await {
            Ok(fired) => {
                for firing in fired {
                    info!(
                        event = %firing.event,
                        session = %firing.session,
                        run = %firing.run,
                        "a trigger fired"
                    );
                }
            }
            Err(error) => warn!(%error, "a firing found nothing it could do"),
        }

        tick(shutdown).await;
    }

    Ok(())
}

async fn tick(shutdown: &CancellationToken) {
    tokio::select! {
        () = tokio::time::sleep(SWEEP) => {}
        () = shutdown.cancelled() => {}
    }
}

/// Every Run found is ended in the transaction that found it, so a heartbeat racing the sweep
/// either got there first — and its Run is not in this read — or waits for the write lock and
/// finds a Run that has ended. A lease that expires fails its Run and never re-dispatches it:
/// kestrel retries dispatch, never work.
async fn sweep(store: &Store) -> Result<Vec<(RunId, Exit)>> {
    let mut tx = store.begin().await?;
    let mut expired = Vec::new();

    for run in tx.expired_leases(Timestamp::now()).await? {
        let exit = Exit::Failed {
            because: "the environment stopped holding the run's lease out, and it expired"
                .to_owned(),
        };
        expired.push((run.id, work::ending(&mut tx, &run, exit).await?));
    }
    tx.commit().await?;

    Ok(expired)
}

/// One at a time, so the write lock is held for a poll's transaction rather than for its wait
/// on the network, and so an Integration whose poll runs long is not asked again underneath
/// the one in flight.
async fn poll(store: &Store, github: &Github) -> Result<()> {
    let due = {
        let mut tx = store.begin().await?;
        tx.integrations_due(Timestamp::now()).await?
    };

    for integration in due {
        let Polled { seen, recorded } = integration::poll(store, github, &integration).await?;
        if recorded > 0 {
            info!(
                integration = integration.name,
                repository = integration.repository,
                seen,
                recorded,
                "a poll recorded events"
            );
        }
    }

    Ok(())
}
