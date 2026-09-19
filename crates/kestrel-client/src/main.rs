mod api;
mod scope;
mod sse;
mod transcript;

use std::io::Read as _;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Parser, Subcommand};
use reqwest::Url;
use serde_json::{Value, json};

use crate::api::ControlPlane;

const NAME: &str = "kestrel-client";
const CONTROL_PLANE: &str = "KESTREL_CONTROL_PLANE";
const DEFAULT_CONTROL_PLANE: &str = "http://127.0.0.1:7718";

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
    #[arg(long, global = true, value_name = "URL")]
    control_plane: Option<String>,

    /// The Organization this invocation applies to; without it, KESTREL_ORGANIZATION, a
    /// committed .kestrel/organization in the working directory, then the only Organization
    /// are tried in order
    #[arg(long, global = true, value_name = "NAME")]
    organization: Option<String>,
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
    /// Declare, inspect, test and control Triggers
    #[command(subcommand)]
    Trigger(TriggerCommand),
    /// Read Sessions
    #[command(subcommand)]
    Session(SessionCommand),
    /// Enqueue and list Runs
    #[command(subcommand)]
    Run(RunCommand),
    /// Print the resolved scope, where each value came from, what exists in it, and what to
    /// run next
    Status,
}

impl Command {
    /// Whether this command's operation is scoped to an Organization, and so resolves the
    /// invocation's scope before it asks the control plane anything.
    fn scoped(&self) -> bool {
        !matches!(
            self,
            Command::Organization(_)
                | Command::Event(EventCommand::Show { .. })
                | Command::Session(
                    SessionCommand::Show { .. }
                        | SessionCommand::Post { .. }
                        | SessionCommand::Seal { .. }
                        | SessionCommand::Transcript { .. },
                )
                | Command::Run(_)
        )
    }
}

#[derive(Debug, Subcommand)]
enum CredentialCommand {
    /// Hold a Provider Credential against an Organization, read from standard input
    Set {
        /// The environment variable an Agent Runtime reads it from
        variable: String,
    },
    /// List what the Organization holds, by the variable each is read from and never by value
    List,
    /// Forget a Provider Credential the Organization holds
    Forget {
        /// The environment variable it is read from
        variable: String,
    },
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    /// Declare a Subscription Profile: a person's login to a subscribed Agent Runtime
    Declare {
        /// The name a Session or Trigger names it by
        name: String,
        /// The person it belongs to, which never changes
        #[arg(long)]
        owner: String,
    },
    /// Hold a login in a profile, read from standard input
    Set {
        /// The profile it is held in
        name: String,
        #[command(flatten)]
        entry: ProfileEntry,
    },
    /// List every profile in the Organization with what each holds, one JSON record a line
    List,
    /// Forget a login a profile holds
    Forget {
        /// The profile it is held in
        name: String,
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
    /// List every Integration in the Organization, one JSON record a line
    List,
    /// Acknowledge the latest oversized Event refused by an Integration
    AcknowledgeRefusal { name: String },
}

#[derive(Debug, Subcommand)]
enum RegisterCommand {
    /// A connection to GitHub, watching one repository
    Github {
        /// The name it is referred to by
        name: String,
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
    /// List the Events recorded for the Organization, most recent first, one JSON record a line
    List {
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
enum TriggerCommand {
    /// Declare a Trigger, or make the named one what this declaration describes
    Declare {
        /// The name it is referred to by
        name: String,
        /// The Events it matches: a CloudEvents filter as JSON
        #[arg(long, value_name = "JSON", required_unless_present = "every")]
        filter: Option<String>,
        /// Fire on a schedule in place of a filter
        #[arg(long, value_name = "DURATION", conflicts_with = "filter")]
        every: Option<String>,
        /// The Brief a firing hands its Session, rendered over an Event
        #[arg(long)]
        brief: String,
        /// The branch a firing's work happens on, rendered from the Event
        #[arg(long)]
        branch: Option<String>,
        /// The key that finds an open Session for this work, rendered from the Event
        #[arg(long)]
        correlation: Option<String>,
        /// What to do when correlation finds no open Session: open or ignore
        #[arg(long, value_name = "OPEN|IGNORE")]
        on_miss: Option<String>,
        /// The Workspace a firing's work happens against
        #[arg(long)]
        workspace: String,
        /// The Agent a firing starts work with
        #[arg(long)]
        agent: String,
        /// Another Agent an agent:<name> label may choose instead; repeat for many
        #[arg(long = "allow", value_name = "AGENT")]
        allows: Vec<String>,
        /// The Subscription Profile a firing's Runs use
        #[arg(long)]
        profile: Option<String>,
    },
    /// List every Trigger in the Organization, one JSON record a line
    List,
    /// Show a Trigger
    Show { name: String },
    /// Say whether a Trigger matches an Event and what it would render, starting no work
    Test {
        name: String,
        /// The Event's record; absent tests the next elapsing of a scheduled Trigger
        #[arg(long)]
        event: Option<String>,
        /// The instruction a dispatch supplies for the Brief to render
        #[arg(long)]
        instruction: Option<String>,
    },
    /// Stop a Trigger firing, without forgetting it
    Disable { name: String },
    /// Let a disabled Trigger fire again
    Enable { name: String },
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
        /// A repository the work happens against; repeat for many
        #[arg(long = "repository", value_name = "URL", required = true)]
        repositories: Vec<String>,
        /// The branch the work happens on
        #[arg(long)]
        branch: String,
    },
    /// List every Workspace in the Organization, one JSON record a line
    List,
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// Declare an Agent, or make the one by this name what this declaration describes
    Declare {
        /// The name it is referred to by
        name: String,
        /// The Agent Runtime that drives it
        #[arg(long, default_value = "opencode")]
        runtime: String,
        /// The model it works with; left out, it names none and its Agent Runtime's default
        /// is the answer
        #[arg(long)]
        model: Option<String>,
    },
    /// List every Agent in the Organization, one JSON record a line
    List,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// Open a Session against a Workspace and an Agent
    Open {
        /// The Workspace its work happens against
        #[arg(long)]
        workspace: String,
        /// The Agent that participates in it
        #[arg(long)]
        agent: String,
        /// The Subscription Profile its Runs use
        #[arg(long)]
        profile: Option<String>,
        /// The branch its work happens on
        #[arg(long)]
        branch: Option<String>,
        /// The sealed Session this one carries on from
        #[arg(long, value_name = "SESSION")]
        continues: Option<String>,
    },
    /// List every Session in the Organization
    List,
    /// Show a Session
    Show {
        /// The Session's identifier
        session: String,
    },
    /// Add a participant's message; starts a Run or queues its next Turn
    Post {
        /// The Session's identifier
        session: String,
        /// The participant saying the message
        #[arg(long, default_value = "operator")]
        as_participant: String,
        /// What the participant says
        message: String,
    },
    /// Seal a Session: readable ever after, and never reopened
    Seal {
        /// The Session's identifier
        session: String,
    },
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

#[derive(Debug, Subcommand)]
enum RunCommand {
    /// Enqueue a Run in a Session, for the work role to claim and dispatch
    Enqueue {
        /// The Session it executes on behalf of
        #[arg(long)]
        session: String,
        /// The model it works with, or none for its Agent's or Agent Runtime's default
        #[arg(long)]
        model: Option<String>,
    },
    /// List every Run in a Session
    List {
        /// The Session the Runs execute on behalf of
        #[arg(long)]
        session: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let client = Client::parse();
    let (control_plane_text, control_plane_source) =
        control_plane(client.control_plane.as_deref())?;
    let control_plane: Url = control_plane_text
        .parse()
        .with_context(|| format!("{control_plane_text} is no control-plane URL"))?;

    let api = ControlPlane::at(control_plane.clone());

    let scope = if client.command.scoped() {
        Some(scope::resolve(&api, client.organization.as_deref()).await?)
    } else {
        None
    };
    let organization = || {
        scope
            .as_ref()
            .expect("a scoped command resolved its scope")
            .organization
            .as_str()
    };

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
            repositories,
            branch,
        }) => {
            let organization = organization();
            let declaration = json!({
                "name": name,
                "repositories": repositories,
                "branch": branch,
            });
            printed(
                &api.post(&["organizations", organization, "workspaces"], &declaration)
                    .await?,
            );
        }
        Command::Workspace(WorkspaceCommand::List) => {
            let organization = organization();
            listed(
                &api.get(&["organizations", organization, "workspaces"])
                    .await?,
            );
        }
        Command::Agent(AgentCommand::Declare {
            name,
            runtime,
            model,
        }) => {
            let organization = organization();
            let declaration = json!({
                "name": name,
                "runtime": runtime,
                "model": model,
            });
            printed(
                &api.post(&["organizations", organization, "agents"], &declaration)
                    .await?,
            );
        }
        Command::Agent(AgentCommand::List) => {
            let organization = organization();
            listed(&api.get(&["organizations", organization, "agents"]).await?);
        }
        Command::Credential(CredentialCommand::Set { variable }) => {
            let organization = organization();
            let secret = json!({ "secret": read_the_secret()? });
            printed(
                &api.put(
                    &["organizations", organization, "credentials", &variable],
                    &secret,
                )
                .await?,
            );
        }
        Command::Credential(CredentialCommand::List) => {
            let organization = organization();
            listed(
                &api.get(&["organizations", organization, "credentials"])
                    .await?,
            );
        }
        Command::Credential(CredentialCommand::Forget { variable }) => {
            let organization = organization();
            api.delete(&["organizations", organization, "credentials", &variable])
                .await?;
        }
        Command::Profile(ProfileCommand::Declare { name, owner }) => {
            let organization = organization();
            printed(
                &api.post(
                    &["organizations", organization, "profiles"],
                    &json!({ "name": name, "owner": owner }),
                )
                .await?,
            );
        }
        Command::Profile(ProfileCommand::Set { name, entry }) => {
            let organization = organization();
            let login = json!({ "secret": read_the_login(entry.file.is_some())? });
            printed(&api.put(&entry.path(organization, &name), &login).await?);
        }
        Command::Profile(ProfileCommand::List) => {
            let organization = organization();
            listed(
                &api.get(&["organizations", organization, "profiles"])
                    .await?,
            );
        }
        Command::Profile(ProfileCommand::Forget { name, entry }) => {
            let organization = organization();
            api.delete(&entry.path(organization, &name)).await?;
        }
        Command::Integration(IntegrationCommand::Register(register)) => {
            let organization = organization();
            let registration = match register {
                RegisterCommand::Github {
                    name,
                    repository,
                    token,
                    carries,
                    interval,
                    webhook_secret,
                    api,
                } => json!({
                    "kind": "github",
                    "name": name,
                    "repository": repository,
                    "token": token,
                    "carries": carries,
                    "interval": interval,
                    "webhook_secret": webhook_secret,
                    "api": api,
                }),
                RegisterCommand::Webhook { name, secret } => {
                    json!({ "kind": "webhook", "name": name, "secret": secret })
                }
            };
            printed(
                &api.post(
                    &["organizations", organization, "integrations"],
                    &registration,
                )
                .await?,
            );
        }
        Command::Integration(IntegrationCommand::List) => {
            let organization = organization();
            listed(
                &api.get(&["organizations", organization, "integrations"])
                    .await?,
            );
        }
        Command::Integration(IntegrationCommand::AcknowledgeRefusal { name }) => {
            let organization = organization();
            api.delete(&[
                "organizations",
                organization,
                "integrations",
                &name,
                "event-refusal",
            ])
            .await?;
        }
        Command::Event(EventCommand::List { limit }) => {
            let organization = organization();
            listed(
                &api.get_where(
                    &["organizations", organization, "events"],
                    &[("limit", &limit.to_string())],
                )
                .await?,
            );
        }
        Command::Event(EventCommand::Show { record }) => {
            printed(&api.get(&["events", &record]).await?);
        }
        Command::Trigger(TriggerCommand::Declare {
            name,
            filter,
            every,
            brief,
            branch,
            correlation,
            on_miss,
            workspace,
            agent,
            allows,
            profile,
        }) => {
            let organization = organization();
            let filter = filter
                .map(|filter| {
                    serde_json::from_str::<Value>(&filter).context("a trigger filter is JSON")
                })
                .transpose()?;
            let mut declaration = json!({
                "name": name,
                "brief": brief,
                "branch": branch,
                "correlation": correlation,
                "on_miss": on_miss,
                "workspace": workspace,
                "agent": agent,
                "allows": allows,
                "profile": profile,
            });
            let declaration = declaration
                .as_object_mut()
                .expect("a trigger declaration is an object");
            if let Some(filter) = filter {
                declaration.insert("filter".to_owned(), filter);
            }
            if let Some(every) = every {
                declaration.insert("every".to_owned(), Value::String(every));
            }
            printed(
                &api.post(&["organizations", organization, "triggers"], &declaration)
                    .await?,
            );
        }
        Command::Trigger(TriggerCommand::List) => {
            let organization = organization();
            listed(
                &api.get(&["organizations", organization, "triggers"])
                    .await?,
            );
        }
        Command::Trigger(TriggerCommand::Show { name }) => {
            let organization = organization();
            printed(
                &api.get(&["organizations", organization, "triggers", &name])
                    .await?,
            );
        }
        Command::Trigger(TriggerCommand::Test {
            name,
            event,
            instruction,
        }) => {
            let organization = organization();
            printed(
                &api.post(
                    &["organizations", organization, "triggers", &name, "test"],
                    &json!({ "event": event, "instruction": instruction }),
                )
                .await?,
            );
        }
        Command::Trigger(TriggerCommand::Disable { name }) => {
            let organization = organization();
            printed(
                &api.post(
                    &["organizations", organization, "triggers", &name, "disable"],
                    &json!({}),
                )
                .await?,
            );
        }
        Command::Trigger(TriggerCommand::Enable { name }) => {
            let organization = organization();
            printed(
                &api.post(
                    &["organizations", organization, "triggers", &name, "enable"],
                    &json!({}),
                )
                .await?,
            );
        }
        Command::Session(SessionCommand::Open {
            workspace,
            agent,
            profile,
            branch,
            continues,
        }) => {
            let organization = organization();
            printed(
                &api.post(
                    &["organizations", organization, "sessions"],
                    &json!({
                        "workspace": workspace,
                        "agent": agent,
                        "profile": profile,
                        "branch": branch,
                        "continues": continues,
                    }),
                )
                .await?,
            );
        }
        Command::Session(SessionCommand::List) => {
            let organization = organization();
            listed(
                &api.get(&["organizations", organization, "sessions"])
                    .await?,
            );
        }
        Command::Session(SessionCommand::Show { session }) => {
            printed(&api.get(&["sessions", &session]).await?);
        }
        Command::Session(SessionCommand::Post {
            session,
            as_participant,
            message,
        }) => {
            printed(
                &api.post(
                    &["sessions", &session, "messages"],
                    &json!({ "participant": as_participant, "message": message }),
                )
                .await?,
            );
        }
        Command::Session(SessionCommand::Seal { session }) => {
            printed(
                &api.post(&["sessions", &session, "seal"], &json!({}))
                    .await?,
            );
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
        Command::Run(RunCommand::Enqueue { session, model }) => {
            printed(
                &api.post(&["sessions", &session, "runs"], &json!({ "model": model }))
                    .await?,
            );
        }
        Command::Run(RunCommand::List { session }) => {
            listed(&api.get(&["sessions", &session, "runs"]).await?);
        }
        Command::Status => {
            let scope = scope.as_ref().expect("a scoped command resolved its scope");
            status(&api, &control_plane_text, control_plane_source, scope).await?;
        }
    }

    Ok(())
}

/// The address to reach, and whether it came from the flag, the environment, or the default.
fn control_plane(given: Option<&str>) -> Result<(String, &'static str)> {
    if let Some(given) = given {
        let url = given.trim();
        if url.is_empty() {
            bail!("--control-plane names no URL");
        }

        return Ok((url.to_owned(), "--control-plane"));
    }
    if let Some(url) = std::env::var(CONTROL_PLANE)
        .ok()
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
    {
        return Ok((url, CONTROL_PLANE));
    }

    Ok((DEFAULT_CONTROL_PLANE.to_owned(), "default"))
}

/// Every resolved value, where it came from, what exists in the scope, and the next command
/// worth running.
async fn status(
    api: &ControlPlane,
    control_plane: &str,
    control_plane_source: &str,
    scope: &scope::Scope,
) -> Result<()> {
    let organization = scope.organization.as_str();
    let workspaces = api
        .get(&["organizations", organization, "workspaces"])
        .await?;
    let agents = api.get(&["organizations", organization, "agents"]).await?;
    let triggers = api
        .get(&["organizations", organization, "triggers"])
        .await?;
    let sessions = api
        .get(&["organizations", organization, "sessions"])
        .await?;
    let integrations = api
        .get(&["organizations", organization, "integrations"])
        .await?;
    let credentials = api
        .get(&["organizations", organization, "credentials"])
        .await?;
    let profiles = api
        .get(&["organizations", organization, "profiles"])
        .await?;

    let workspace_names = names(&workspaces);
    let agent_names = names(&agents);

    printed(&json!({
        "control_plane": control_plane,
        "control_plane_source": control_plane_source,
        "organization": &scope.organization,
        "organization_source": &scope.source,
        "binding": scope.binding.as_ref().map(|path| path.display().to_string()),
        "workspaces": workspace_names.len(),
        "agents": agent_names.len(),
        "triggers": names(&triggers).len(),
        "sessions": sessions.as_array().map_or(0, Vec::len),
        "integrations": names(&integrations).len(),
        "credentials": credentials.as_array().map_or(0, Vec::len),
        "profiles": profiles.as_array().map_or(0, Vec::len),
        "next": next_command(&workspace_names, &agent_names),
    }));

    Ok(())
}

fn names(records: &Value) -> Vec<String> {
    records
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|record| record["name"].as_str().map(str::to_owned))
        .collect()
}

fn next_command(workspaces: &[String], agents: &[String]) -> String {
    match (workspaces.first(), agents.first()) {
        (Some(workspace), Some(agent)) => {
            format!("{NAME} session open --workspace {workspace} --agent {agent}")
        }
        (Some(_), None) => format!("{NAME} agent declare <name>"),
        (None, _) => format!("{NAME} workspace declare <name> --repository <url> --branch main"),
    }
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
