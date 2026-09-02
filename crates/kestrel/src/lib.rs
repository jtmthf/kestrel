pub mod cli;
pub mod compute;
pub mod domain;
pub mod fanout;
pub mod link;
pub mod log;
pub mod role;
pub mod session;
pub mod shutdown;
pub mod store;
pub mod telemetry;
pub mod timer;
pub mod work;

use tokio_util::sync::CancellationToken;

use crate::cli::{Cli, Selection};
use crate::store::Store;

pub async fn run(cli: &Cli, shutdown: CancellationToken) -> anyhow::Result<()> {
    let store = Store::open(&cli.data_dir()?).await?;

    match cli.selection() {
        Selection::AllInOne => {
            let all_in_one = role::bind(store, cli.listen).await?;
            let dispatch = cli.dispatch(all_in_one.address())?;
            all_in_one.run(Some(dispatch), shutdown).await
        }
        Selection::Serve => {
            let listening = role::serve::bind(store, cli.listen).await?;
            role::serve::run(listening, shutdown).await
        }
        Selection::Work => {
            let dispatch = cli.dispatch(cli.listen)?;
            role::work::run(store, Some(dispatch), shutdown).await
        }
        Selection::Cli(command) => role::cli::run(command, store).await,
    }
}
