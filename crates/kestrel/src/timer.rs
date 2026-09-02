//! The in-process wheel over the due-time index `Store` holds (ADR-0005). The schedule is
//! never in process memory alone, so a control plane that restarts finds every due time that
//! was set before it existed and fires it.

use std::time::Duration;

use anyhow::Result;
use jiff::Timestamp;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::store::Store;
use crate::work;

/// Nothing subscribes to `Fanout` at 0.1 (ADR-0005), so a due time that has come round is
/// found by asking `Store` again rather than by being told.
const SWEEP: Duration = Duration::from_millis(500);

pub async fn sweeping(store: &Store, shutdown: &CancellationToken) -> Result<()> {
    while !shutdown.is_cancelled() {
        // The database being busy is not a reason to stop keeping time: the same due times
        // are still there to be found on the next sweep.
        if let Err(error) = sweep(store).await {
            warn!(%error, "a sweep found nothing it could do");
        }

        tokio::select! {
            () = tokio::time::sleep(SWEEP) => {}
            () = shutdown.cancelled() => {}
        }
    }

    Ok(())
}

/// A lease that expired fails its Run and is never re-dispatched: kestrel retries dispatch,
/// never work.
pub async fn sweep(store: &Store) -> Result<()> {
    let mut tx = store.begin().await?;
    let expired = tx.expired_leases(Timestamp::now()).await?;
    drop(tx);

    for run in expired {
        let exit = work::fail(
            store,
            &run,
            "the environment stopped holding the run's lease out, and it expired",
        )
        .await?;
        info!(run = %run.id, %exit, "a lease expired");
    }

    Ok(())
}
