//! The in-process wheel (ADR-0005). The schedule is never in process memory alone, so a
//! control plane that restarts finds every due time set before it existed and fires it.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use jiff::Timestamp;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::domain::{Exit, RunId};
use crate::follow_up;
use crate::integration::delivery;
use crate::integration::github::Github;
use crate::integration::{self, Polled};
use crate::session;
use crate::store::Store;
use crate::trigger;
use crate::work;

const SWEEP: Duration = Duration::from_millis(500);

/// Starts the sweeps that consume Events now rather than at their next tick. It reaches only a
/// work role in the same process; across processes the sweep interval is the guarantee.
#[derive(Clone)]
pub struct Wake(Arc<watch::Sender<()>>);

impl Default for Wake {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(())))
    }
}

impl Wake {
    pub fn wake(&self) {
        self.0.send_replace(());
    }
}

pub async fn sweeping(store: &Store, wake: &Wake, shutdown: &CancellationToken) -> Result<()> {
    let github = Github::dialling_out()?;

    // Beside the lease sweep rather than in it: a poll waits on GitHub, and a lease left
    // unswept for the length of an HTTP request is a Session wedged for that long.
    tokio::try_join!(
        sweeping_leases(store, shutdown),
        polling(store, &github, shutdown),
        elapsing(store, wake, shutdown),
        firing(store, &github, wake.0.subscribe(), shutdown),
        following_up(store, wake.0.subscribe(), shutdown),
        sealing_idle_sessions(store, shutdown),
        delivering(store, &github, shutdown)
    )?;

    Ok(())
}

async fn following_up(
    store: &Store,
    mut woken: watch::Receiver<()>,
    shutdown: &CancellationToken,
) -> Result<()> {
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

        tick_or_woken(shutdown, &mut woken).await;
    }

    Ok(())
}

async fn elapsing(store: &Store, wake: &Wake, shutdown: &CancellationToken) -> Result<()> {
    while !shutdown.is_cancelled() {
        match trigger::elapse(store, Timestamp::now()).await {
            Ok(minted) => {
                for occurrence in &minted {
                    info!(source = occurrence.source, due = %occurrence.time, "a schedule elapsed");
                }
                if !minted.is_empty() {
                    wake.wake();
                }
            }
            Err(error) => warn!(%error, "a schedule sweep found nothing it could do"),
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

/// Matching is its own sweep rather than the tail of a poll, so a control plane that stopped
/// between recording an Event and firing for it finds it on the way back up.
async fn firing(
    store: &Store,
    github: &Github,
    mut woken: watch::Receiver<()>,
    shutdown: &CancellationToken,
) -> Result<()> {
    while !shutdown.is_cancelled() {
        match trigger::fire(store, github).await {
            Ok(fired) => {
                for firing in fired {
                    match firing {
                        trigger::Fired::Opened {
                            event,
                            session,
                            run,
                        } => info!(%event, %session, %run, "a trigger fired"),
                        trigger::Fired::Fed {
                            event,
                            session,
                            run,
                        } => info!(%event, %session, ?run, "a trigger fed a session"),
                        trigger::Fired::Ignored {
                            event,
                            trigger,
                            correlation,
                        } => {
                            info!(%event, %trigger, %correlation, "a trigger ignored a correlation miss")
                        }
                        trigger::Fired::Failed {
                            event,
                            trigger,
                            because,
                        } => {
                            warn!(%event, %trigger, %because, "a trigger fired and opened nothing")
                        }
                        trigger::Fired::Held {
                            event,
                            trigger,
                            because,
                        } => {
                            info!(%event, %trigger, %because, "a trigger held work")
                        }
                    }
                }
            }
            Err(error) => warn!(%error, "a firing found nothing it could do"),
        }

        tick_or_woken(shutdown, &mut woken).await;
    }

    Ok(())
}

async fn tick(shutdown: &CancellationToken) {
    tokio::select! {
        () = tokio::time::sleep(SWEEP) => {}
        () = shutdown.cancelled() => {}
    }
}

/// A wake that arrives mid-sweep is kept, so the Event it announced is not left to the tick.
async fn tick_or_woken(shutdown: &CancellationToken, woken: &mut watch::Receiver<()>) {
    tokio::select! {
        () = tokio::time::sleep(SWEEP) => {}
        _ = woken.changed() => {}
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
            because: "the supervisor stopped holding the run's lease out, and it expired"
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
        tx.integrations().deliveries_due(Timestamp::now()).await?
    };

    for delivery in due {
        if let Some(comment) = delivery::deliver(store, github, &delivery).await? {
            info!(
                run = %delivery.run,
                turn = delivery.turn,
                comment,
                "what a run said reached the issue it came from"
            );
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
                seen, recorded, "a poll recorded events"
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[tokio::test]
    async fn a_wake_ends_the_wait_before_the_tick() {
        let wake = Wake::default();
        let mut woken = wake.0.subscribe();
        let started = Instant::now();

        wake.wake();
        tick_or_woken(&CancellationToken::new(), &mut woken).await;

        assert!(started.elapsed() < SWEEP / 2);
    }

    #[tokio::test]
    async fn without_a_wake_the_tick_is_the_floor() {
        let wake = Wake::default();
        let mut woken = wake.0.subscribe();
        let started = Instant::now();

        tick_or_woken(&CancellationToken::new(), &mut woken).await;

        assert!(started.elapsed() >= SWEEP);
    }

    #[tokio::test]
    async fn one_wake_reaches_every_sweep_waiting_on_it() {
        let wake = Wake::default();
        let mut firing = wake.0.subscribe();
        let mut following_up = wake.0.subscribe();
        let shutdown = CancellationToken::new();
        let started = Instant::now();

        wake.wake();
        tick_or_woken(&shutdown, &mut firing).await;
        tick_or_woken(&shutdown, &mut following_up).await;

        assert!(started.elapsed() < SWEEP / 2);
    }
}
