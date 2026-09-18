mod api;
mod sse;
mod transcript;

use std::io::Read as _;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Parser, Subcommand};
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
    /// Hold, list and forget the Provider Credentials an Organization's Runs reach a model with
    #[command(subcommand)]
    Credential(CredentialCommand),
    /// Declare Subscription Profiles, and hold, list and forget the logins in them
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Register and list Integrations: credentialed connections to external systems
    #[command(subcommand)]
    Integration(IntegrationCommand),
    /// Read the Events recorded for an Organization
    #[command(subcommand)]
    Event(EventCommand),
    /// Read Sessions
    #[command(subcommand)]
    Session(SessionCommand),
}

#[derive(Debug, Subcommand)]
enum CredentialCommand {
    /// Hold a Provider Credential against an Organization, read from standard input
    Set {
        /// The environment variable an Agent Runtime reads it from
        variable: String,
        /// The Organization that holds it
        #[arg(long)]
        organization: String,
    },
    /// List what an Organization holds, by the variable each is read from and never by value
    List {
        #[arg(long)]
        organization: String,
    },
    /// Forget a Provider Credential an Organization holds
    Forget {
        /// The environment variable it is read from
        variable: String,
        /// The Organization that holds it
        #[arg(long)]
        organization: String,
    },
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    /// Declare a Subscription Profile: a person's login to a subscribed Agent Runtime
    Declare {
        /// The name a Session or Trigger names it by
        name: String,
        /// The Organization it is declared in
        #[arg(long)]
        organization: String,
        /// The person it belongs to, which never changes
        #[arg(long)]
        owner: String,
    },
    /// Hold a login in a profile, read from standard input
    Set {
        /// The profile it is held in
        name: String,
        #[arg(long)]
        organization: String,
        #[command(flatten)]
        entry: ProfileEntry,
    },
    /// List every profile in an Organization with what each holds, one JSON record a line
    List {
        #[arg(long)]
        organization: String,
    },
    /// Forget a login a profile holds
    Forget {
        /// The profile it is held in
        name: String,
        #[arg(long)]
        organization: String,
        #[command(flatten)]
        entry: ProfileEntry,
    },
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct ProfileEntry {
    /// An environment variable the Agent Runtime is spawned with
    #[arg(long, value_name = "NAME")]
    variable: Option<String>,
    /// A file beneath the agent's home, handed back after each Run so a refreshed login
    /// persists
    #[arg(long, value_name = "PATH")]
    file: Option<String>,
}

impl ProfileEntry {
    fn path<'a>(&'a self, organization: &'a str, profile: &'a str) -> Vec<&'a str> {
        let mut path = vec!["organizations", organization, "profiles", profile];
        match (&self.variable, &self.file) {
            (Some(variable), _) => path.extend(["variables", variable.as_str()]),
            (None, Some(file)) => path.extend(["files", file.as_str()]),
            (None, None) => unreachable!("clap requires one of them"),
        }

        path
    }
}

#[derive(Debug, Subcommand)]
enum IntegrationCommand {
    /// Register an Integration
    #[command(subcommand)]
    Register(RegisterCommand),
    /// List every Integration in an Organization, one JSON record a line
    List {
        #[arg(long)]
        organization: String,
    },
    /// Acknowledge the latest oversized Event refused by an Integration
    AcknowledgeRefusal {
        name: String,
        #[arg(long)]
        organization: String,
    },
}

#[derive(Debug, Subcommand)]
enum RegisterCommand {
    /// A connection to GitHub, watching one repository
    Github {
        /// The name it is referred to by
        name: String,
        /// The Organization whose credential it holds
        #[arg(long)]
        organization: String,
        /// The repository it watches, as owner/name
        #[arg(long, value_name = "OWNER/NAME")]
        repository: String,
        /// The credential it presents to GitHub
        #[arg(
            long,
            env = "KESTREL_GITHUB_TOKEN",
            value_name = "TOKEN",
            hide_env_values = true
        )]
        token: String,
        /// A direction it carries — inbound, outbound; repeat for both
        #[arg(
            long = "carries",
            value_name = "DIRECTION",
            default_values = ["inbound", "outbound"]
        )]
        carries: Vec<String>,
        /// How often the poll asks GitHub what has happened
        #[arg(long, value_name = "DURATION", default_value = "1m")]
        interval: String,
        /// The secret GitHub signs webhook deliveries with; given one, kestrel receives the
        /// repository's events by webhook and stops polling for them
        #[arg(
            long,
            env = "KESTREL_GITHUB_WEBHOOK_SECRET",
            value_name = "SECRET",
            hide_env_values = true
        )]
        webhook_secret: Option<String>,
        #[arg(long, env = "KESTREL_GITHUB_API", hide = true)]
        api: Option<String>,
    },
    /// A generic endpoint any producer can POST CloudEvents to
    Webhook {
        /// The name it is referred to by
        name: String,
        /// The Organization whose Events it records
        #[arg(long)]
        organization: String,
        /// The secret a sender presents as `Authorization: Bearer <secret>`
        #[arg(
            long,
            env = "KESTREL_WEBHOOK_SECRET",
            value_name = "SECRET",
            hide_env_values = true
        )]
        secret: String,
    },
}

#[derive(Debug, Subcommand)]
enum EventCommand {
    /// List the Events recorded for an Organization, most recent first, one JSON record a line
    List {
        #[arg(long)]
        organization: String,
        /// How many to list at most
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show one Event's whole envelope and payload
    Show {
        /// The Event's record identifier
        record: String,
    },
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
        Command::Credential(CredentialCommand::Set {
            variable,
            organization,
        }) => {
            let secret = json!({ "secret": read_the_secret()? });
            printed(
                &api.put(
                    &["organizations", &organization, "credentials", &variable],
                    &secret,
                )
                .await?,
            );
        }
        Command::Credential(CredentialCommand::List { organization }) => {
            listed(
                &api.get(&["organizations", &organization, "credentials"])
                    .await?,
            );
        }
        Command::Credential(CredentialCommand::Forget {
            variable,
            organization,
        }) => {
            api.delete(&["organizations", &organization, "credentials", &variable])
                .await?;
        }
        Command::Profile(ProfileCommand::Declare {
            name,
            organization,
            owner,
        }) => {
            printed(
                &api.post(
                    &["organizations", &organization, "profiles"],
                    &json!({ "name": name, "owner": owner }),
                )
                .await?,
            );
        }
        Command::Profile(ProfileCommand::Set {
            name,
            organization,
            entry,
        }) => {
            let login = json!({ "secret": read_the_login(entry.file.is_some())? });
            printed(&api.put(&entry.path(&organization, &name), &login).await?);
        }
        Command::Profile(ProfileCommand::List { organization }) => {
            listed(
                &api.get(&["organizations", &organization, "profiles"])
                    .await?,
            );
        }
        Command::Profile(ProfileCommand::Forget {
            name,
            organization,
            entry,
        }) => {
            api.delete(&entry.path(&organization, &name)).await?;
        }
        Command::Integration(IntegrationCommand::Register(register)) => {
            let (organization, registration) = match register {
                RegisterCommand::Github {
                    name,
                    organization,
                    repository,
                    token,
                    carries,
                    interval,
                    webhook_secret,
                    api,
                } => (
                    organization,
                    json!({
                        "kind": "github",
                        "name": name,
                        "repository": repository,
                        "token": token,
                        "carries": carries,
                        "interval": interval,
                        "webhook_secret": webhook_secret,
                        "api": api,
                    }),
                ),
                RegisterCommand::Webhook {
                    name,
                    organization,
                    secret,
                } => (
                    organization,
                    json!({ "kind": "webhook", "name": name, "secret": secret }),
                ),
            };
            printed(
                &api.post(
                    &["organizations", &organization, "integrations"],
                    &registration,
                )
                .await?,
            );
        }
        Command::Integration(IntegrationCommand::List { organization }) => {
            listed(
                &api.get(&["organizations", &organization, "integrations"])
                    .await?,
            );
        }
        Command::Integration(IntegrationCommand::AcknowledgeRefusal { name, organization }) => {
            api.delete(&[
                "organizations",
                &organization,
                "integrations",
                &name,
                "event-refusal",
            ])
            .await?;
        }
        Command::Event(EventCommand::List {
            organization,
            limit,
        }) => {
            listed(
                &api.get_where(
                    &["organizations", &organization, "events"],
                    &[("limit", &limit.to_string())],
                )
                .await?,
            );
        }
        Command::Event(EventCommand::Show { record }) => {
            printed(&api.get(&["events", &record]).await?);
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

/// A file is held exactly as it was read, because it is written back exactly as it was held.
fn read_the_login(file: bool) -> Result<String> {
    let mut read = String::new();
    std::io::stdin().read_to_string(&mut read)?;

    if read.trim().is_empty() {
        bail!("a login is read from standard input, and nothing was on it");
    }

    Ok(if file { read } else { read.trim().to_owned() })
}

/// Off standard input rather than out of an argument, so a provider's key is never in a shell
/// history or in what `ps` shows of this process.
fn read_the_secret() -> Result<String> {
    let mut read = String::new();
    std::io::stdin().read_to_string(&mut read)?;
    let secret = read.trim();

    if secret.is_empty() {
        bail!("a provider credential is read from standard input, and nothing was on it");
    }

    Ok(secret.to_owned())
}
