mod sse;
mod transcript;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use reqwest::Url;

#[derive(Debug, Parser)]
#[command(
    name = "kestrel-client",
    version,
    about = "Reach a kestrel control plane over its operator boundary.",
    disable_help_subcommand = true
)]
struct Client {
    #[command(subcommand)]
    command: Command,

    /// The control plane's operator boundary
    #[arg(
        long,
        env = "KESTREL_CONTROL_PLANE",
        global = true,
        value_name = "URL",
        default_value = "http://127.0.0.1:7718"
    )]
    control_plane: String,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read Sessions
    #[command(subcommand)]
    Session(SessionCommand),
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// Read a Session's Transcript, one JSON entry a line, and the cursor a later read
    /// resumes from
    Transcript {
        /// The Session's identifier
        session: String,
        /// Resume after the cursor a previous read ended with
        #[arg(long)]
        cursor: Option<String>,
        /// Keep reading as entries are appended, until the Session is sealed
        #[arg(long)]
        follow: bool,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let client = Client::parse();
    let control_plane: Url = client
        .control_plane
        .parse()
        .with_context(|| format!("{} is no control-plane URL", client.control_plane))?;

    match client.command {
        Command::Session(SessionCommand::Transcript {
            session,
            cursor,
            follow,
        }) => {
            let read = transcript::read(&control_plane, &session, cursor, follow).await?;
            // Beside the Transcript rather than in it, so stdout carries entries and nothing else.
            if let Some(cursor) = read {
                eprintln!("cursor  {cursor}");
            }
        }
    }

    Ok(())
}
