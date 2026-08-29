pub mod cli;
pub mod role;
pub mod shutdown;
pub mod telemetry;

use tokio_util::sync::CancellationToken;

use crate::cli::{Cli, Selection};

pub async fn run(cli: &Cli, shutdown: CancellationToken) -> anyhow::Result<()> {
    match cli.selection() {
        Selection::AllInOne => role::all_in_one(shutdown).await,
        Selection::Serve => role::serve::run(shutdown).await,
        Selection::Work => role::work::run(shutdown).await,
    }
}
