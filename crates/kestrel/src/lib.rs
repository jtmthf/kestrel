pub mod cli;
pub mod compute;
pub mod domain;
pub mod fanout;
pub mod log;
pub mod role;
pub mod session;
pub mod shutdown;
pub mod store;
pub mod telemetry;

use tokio_util::sync::CancellationToken;

use crate::cli::{Cli, Selection};
use crate::store::Store;

pub async fn run(cli: &Cli, shutdown: CancellationToken) -> anyhow::Result<()> {
    let store = Store::open(&cli.data_dir()?).await?;

    match cli.selection() {
        Selection::AllInOne => role::all_in_one(shutdown).await,
        Selection::Serve => role::serve::run(shutdown).await,
        Selection::Work => role::work::run(shutdown).await,
        Selection::Cli(command) => role::cli::run(command, store).await,
    }
}
