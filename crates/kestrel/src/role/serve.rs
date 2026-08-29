//! The `serve` role: the API, and the link an Environment dials out to (ADR-0002).
//!
//! At 0.1 it starts and waits. The link arrives in 0.1/04, the API around it after that.

use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::cli::Role;

pub async fn run(shutdown: CancellationToken) -> anyhow::Result<()> {
    info!(role = %Role::Serve, "role started");
    shutdown.cancelled().await;
    info!(role = %Role::Serve, "role stopped");
    Ok(())
}
