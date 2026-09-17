mod api;
mod sse;
mod transcript;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use reqwest::Url;
use serde_json::{Value, json};

use crate::api::ControlPlane;

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
    /// Declare and list Organizations
    #[command(subcommand)]
    Organization(OrganizationCommand),
    /// Declare and list Workspaces
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Declare and list Agents
    #[command(subcommand)]
    Agent(AgentCommand),
    /// Read Sessions
    #[command(subcommand)]
    Session(SessionCommand),
}

#[derive(Debug, Subcommand)]
enum OrganizationCommand {
    /// Declare an Organization; declaring one that exists changes nothing
    Declare {
        /// The name it is referred to by
        name: String,
    },
    /// List every Organization, one JSON record a line
    List,
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    /// Declare a Workspace, or make the one by this name what this declaration describes
    Declare {
        /// The name it is referred to by
        name: String,
        /// The Organization it belongs to
        #[arg(long)]
        organization: String,
        /// A repository the work happens against; repeat for many
        #[arg(long = "repository", value_name = "URL", required = true)]
        repositories: Vec<String>,
        /// The branch the work happens on
        #[arg(long)]
        branch: String,
    },
    /// List every Workspace in an Organization, one JSON record a line
    List {
        #[arg(long)]
        organization: String,
    },
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// Declare an Agent, or make the one by this name what this declaration describes
    Declare {
        /// The name it is referred to by
        name: String,
        /// The Organization it belongs to
        #[arg(long)]
        organization: String,
        /// The Agent Runtime that drives it
        #[arg(long, default_value = "opencode")]
        runtime: String,
        /// The model it works with; left out, it names none and its Agent Runtime's default
        /// is the answer
        #[arg(long)]
        model: Option<String>,
    },
    /// List every Agent in an Organization, one JSON record a line
    List {
        #[arg(long)]
        organization: String,
    },
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

    let api = ControlPlane::at(control_plane.clone());

    match client.command {
        Command::Organization(OrganizationCommand::Declare { name }) => {
            printed(
                &api.post(&["organizations"], &json!({ "name": name }))
                    .await?,
            );
        }
        Command::Organization(OrganizationCommand::List) => {
            listed(&api.get(&["organizations"]).await?);
        }
        Command::Workspace(WorkspaceCommand::Declare {
            name,
            organization,
            repositories,
            branch,
        }) => {
            let declaration = json!({
                "name": name,
                "repositories": repositories,
                "branch": branch,
            });
            printed(
                &api.post(
                    &["organizations", &organization, "workspaces"],
                    &declaration,
                )
                .await?,
            );
        }
        Command::Workspace(WorkspaceCommand::List { organization }) => {
            listed(
                &api.get(&["organizations", &organization, "workspaces"])
                    .await?,
            );
        }
        Command::Agent(AgentCommand::Declare {
            name,
            organization,
            runtime,
            model,
        }) => {
            let declaration = json!({
                "name": name,
                "runtime": runtime,
                "model": model,
            });
            printed(
                &api.post(&["organizations", &organization, "agents"], &declaration)
                    .await?,
            );
        }
        Command::Agent(AgentCommand::List { organization }) => {
            listed(&api.get(&["organizations", &organization, "agents"]).await?);
        }
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

fn printed(record: &Value) {
    println!("{record}");
}

fn listed(records: &Value) {
    for record in records.as_array().into_iter().flatten() {
        printed(record);
    }
}
