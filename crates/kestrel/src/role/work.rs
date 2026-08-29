//! The `work` role: claiming queued Runs and executing them.
//!
//! At 0.1 it starts and waits. `Work` — enqueue, claim, heartbeat, complete, fail —
//! arrives in 0.1/05, and the lease it heartbeats in 0.1/08.

use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::cli::Role;

pub async fn run(shutdown: CancellationToken) -> anyhow::Result<()> {
    info!(role = %Role::Work, "role started");
    shutdown.cancelled().await;
    info!(role = %Role::Work, "role stopped");
    Ok(())
}
