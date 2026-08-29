use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::cli::Role;

pub async fn run(shutdown: CancellationToken) -> anyhow::Result<()> {
    info!(role = %Role::Serve, "role started");
    shutdown.cancelled().await;
    info!(role = %Role::Serve, "role stopped");
    Ok(())
}
