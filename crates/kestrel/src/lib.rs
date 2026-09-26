pub mod agent;
pub mod cli;
pub mod compute;
pub mod cron;
pub mod declaration;
pub mod declined;
pub mod domain;
pub mod fanout;
pub mod filter;
pub mod follow_up;
pub mod hex;
pub mod instance;
pub mod integration;
pub mod keyring;
pub mod link;
pub mod log;
pub mod operator;
pub mod profile;
pub mod provider;
pub mod readiness;
pub mod reference;
pub mod role;
pub mod shutdown;
pub mod start;
pub mod store;
pub mod telemetry;
pub mod template;
pub mod timer;
pub mod trigger;
pub mod work;
pub mod workspace;

use tokio_util::sync::CancellationToken;

use crate::cli::{Cli, Command};
use crate::store::Store;

pub async fn run(cli: &Cli, shutdown: CancellationToken) -> anyhow::Result<()> {
    let store = Store::open(&cli.data_dir()?).await?;

    match cli.command {
        None => {
            let all_in_one = role::bind(store, cli.listen()).await?;
            let dispatch = cli.dispatch(all_in_one.bound().link)?;
            all_in_one.run(Some(dispatch), shutdown).await
        }
        Some(Command::Serve) => {
            let listening = role::serve::bind(store, cli.listen(), timer::Wake::default()).await?;
            role::serve::run(listening, shutdown).await
        }
        Some(Command::Work) => {
            let dispatch = cli.dispatch(cli.listen)?;
            role::work::run(store, Some(dispatch), timer::Wake::default(), shutdown).await
        }
    }
}
