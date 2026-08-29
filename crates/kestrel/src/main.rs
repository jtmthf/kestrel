use clap::Parser;

use kestrel::cli::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    kestrel::telemetry::init();
    let shutdown = kestrel::shutdown::on_signal()?;
    kestrel::run(&cli, shutdown).await
}
