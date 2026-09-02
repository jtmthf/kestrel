//! The in-process wheel (ADR-0005). The schedule is never in process memory alone, so a
//! control plane that restarts finds every due time set before it existed and fires it.

use std::time::Duration;

use anyhow::Result;
use jiff::Timestamp;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::domain::{Exit, RunId};
use crate::store::Store;
use crate::work;

const SWEEP: Duration = Duration::from_millis(500);

pub async fn sweeping(store: &Store, shutdown: &CancellationToken) -> Result<()> {
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

        tokio::select! {
            () = tokio::time::sleep(SWEEP) => {}
            () = shutdown.cancelled() => {}
        }
    }

    Ok(())
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
