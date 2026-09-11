//! The in-process wheel (ADR-0005). The schedule is never in process memory alone, so a
//! control plane that restarts finds every due time set before it existed and fires it.

use std::time::Duration;

use anyhow::Result;
use jiff::Timestamp;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::domain::{Exit, RunId};
use crate::follow_up;
use crate::integration::github::Github;
use crate::integration::outcome;
use crate::integration::{self, Polled};
use crate::session;
use crate::store::Store;
use crate::trigger;
use crate::work;

const SWEEP: Duration = Duration::from_millis(500);

/// How old an Event must be before the reaping sweep may forget it (ADR-0011).
const RETENTION: jiff::SignedDuration = jiff::SignedDuration::from_secs(90 * 24 * 60 * 60);

pub async fn sweeping(store: &Store, shutdown: &CancellationToken) -> Result<()> {
    let github = Github::dialling_out()?;

    // Beside the lease sweep rather than in it: a poll waits on GitHub, and a lease left
    // unswept for the length of an HTTP request is a Session wedged for that long.
    tokio::try_join!(
        sweeping_leases(store, shutdown),
        polling(store, &github, shutdown),
        firing(store, shutdown),
        following_up(store, shutdown),
        sealing_idle_sessions(store, shutdown),
        delivering(store, &github, shutdown),
        reaping(store, shutdown)
    )?;

    Ok(())
}

async fn following_up(store: &Store, shutdown: &CancellationToken) -> Result<()> {
    while !shutdown.is_cancelled() {
        match follow_up::receive(store).await {
            Ok(received) => {
                for follow_up in received {
                    info!(
                        event = %follow_up.event,
                        session = %follow_up.session,
                        run = ?follow_up.run,
                        "a follow-up was received"
                    );
                }
            }
            Err(error) => warn!(%error, "a follow-up sweep found nothing it could do"),
        }

        tick(shutdown).await;
    }

    Ok(())
}

async fn sealing_idle_sessions(store: &Store, shutdown: &CancellationToken) -> Result<()> {
    while !shutdown.is_cancelled() {
        match session::seal_idle(store).await {
            Ok(sealed) => {
                for session in sealed {
                    info!(session = %session.id, "an idle session sealed itself");
                }
            }
            Err(error) => warn!(%error, "an idle sweep found nothing it could do"),
        }

        tick(shutdown).await;
    }

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

async fn polling(store: &Store, github: &Github, shutdown: &CancellationToken) -> Result<()> {
    while !shutdown.is_cancelled() {
        match poll(store, github).await {
            Ok(()) => {}
            Err(error) => warn!(%error, "a poll found nothing it could do"),
        }

        tick(shutdown).await;
    }

    Ok(())
}

/// A sweep of its own rather than the tail of the transaction that ends a Run: what a Run
/// ended as is durable the moment it ends, and saying so out loud is a request to somebody
/// else's system that may be refused, deferred and asked again without any of that reaching
/// the Run.
async fn delivering(store: &Store, github: &Github, shutdown: &CancellationToken) -> Result<()> {
    while !shutdown.is_cancelled() {
        match deliver(store, github).await {
            Ok(()) => {}
            Err(error) => warn!(%error, "a delivery found nothing it could do"),
        }

        tick(shutdown).await;
    }

    Ok(())
}

/// Reaping forgets an Event only once nothing depends on it, so the event a Session was
/// opened by stays as long as the Session that looks back at it.
async fn reaping(store: &Store, shutdown: &CancellationToken) -> Result<()> {
    while !shutdown.is_cancelled() {
        match reap(store).await {
            Ok(forgotten) => {
                if forgotten > 0 {
                    info!(forgotten, "events older than the retention window expired");
                }
            }
            Err(error) => warn!(%error, "a reaping found nothing it could do"),
        }

        tick(shutdown).await;
    }

    Ok(())
}

async fn reap(store: &Store) -> Result<usize> {
    let mut tx = store.begin().await?;
    let forgotten = tx
        .integrations()
        .reap_events(Timestamp::now() - RETENTION)
        .await?;
    tx.commit().await?;

    Ok(forgotten)
}

/// Matching is its own sweep rather than the tail of a poll, so a control plane that stopped
/// between recording an Event and firing for it finds it on the way back up.
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

    for run in tx.sessions().expired_leases(Timestamp::now()).await? {
        let exit = Exit::Failed {
            because: "the environment stopped holding the run's lease out, and it expired"
                .to_owned(),
        };
        expired.push((run.id, work::ending(&mut tx, &run, exit).await?));
    }
    tx.commit().await?;

    Ok(expired)
}

async fn deliver(store: &Store, github: &Github) -> Result<()> {
    let due = {
        let mut tx = store.begin().await?;
        tx.integrations().outcomes_due(Timestamp::now()).await?
    };

    for outcome in due {
        if let Some(comment) = outcome::deliver(store, github, &outcome).await? {
            info!(run = %outcome.run, comment, "a run's outcome reached the issue it came from");
        }
    }

    Ok(())
}

/// One at a time, so the write lock is held for a poll's transaction rather than for its wait
/// on the network, and so an Integration whose poll runs long is not asked again underneath
/// the one in flight.
async fn poll(store: &Store, github: &Github) -> Result<()> {
    let due = {
        let mut tx = store.begin().await?;
        tx.integrations().due(Timestamp::now()).await?
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
