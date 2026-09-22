mod api;
mod output;
mod scope;
mod sse;
mod transcript;
mod view;

use std::io::{IsTerminal as _, Read as _};

use anyhow::{Context as _, Result, bail};
use clap::builder::NonEmptyStringValueParser;
use clap::parser::ValueSource;
use clap::{Args, CommandFactory as _, FromArgMatches as _, Parser, Subcommand};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::ControlPlane;
use crate::output::{Presentation, show};
use crate::scope::{Derived, Scope, Scoping, Source};

const BINARY: &str = "kestrel-client";
const CONTROL_PLANE_VARIABLE: &str = "KESTREL_CONTROL_PLANE";
const ORGANIZATION_VARIABLE: &str = "KESTREL_ORGANIZATION";

#[derive(Debug, Parser)]
#[command(
    name = BINARY,
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
        env = CONTROL_PLANE_VARIABLE,
        global = true,
        value_name = "URL",
        value_parser = NonEmptyStringValueParser::new(),
        default_value = "http://127.0.0.1:7718"
    )]
    control_plane: String,

    /// The Organization this invocation applies to; without it, a committed
    /// .kestrel/organization between the working directory and its repository root, then the
    /// only Organization
    #[arg(
        long,
        env = ORGANIZATION_VARIABLE,
        global = true,
        value_name = "NAME",
        value_parser = NonEmptyStringValueParser::new()
    )]
    organization: Option<String>,

    /// Emit these fields and no others, as JSON, one record a line; without it a terminal
    /// gets the presentation chosen for the command and anything else gets it tab-delimited
    #[arg(long, global = true, value_name = "FIELDS")]
    json: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Preview and apply one Workspace, Agent and Trigger declaration document
    Apply(Apply),
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
    /// Show, enqueue and list Runs
    #[command(subcommand)]
    Run(RunCommand),
    /// Print the resolved scope, where each value came from, what exists in it, and what to
    /// run next
    Status,
}

#[derive(Debug, Args)]
struct Apply {
    /// The declaration file, or `-` for standard input
    #[arg(short = 'f', long, value_name = "FILE")]
    file: String,
}

impl Command {
    fn scoped(&self) -> bool {
        match self {
            Command::Apply(_)
            | Command::Workspace(_)
            | Command::Agent(_)
            | Command::Credential(_)
            | Command::Profile(_)
            | Command::Integration(_)
            | Command::Trigger(_)
            | Command::Session(_)
            | Command::Run(_)
            | Command::Status => true,
            Command::Organization(_) => false,
            Command::Event(event) => match event {
                EventCommand::List { .. } => true,
                EventCommand::Show { .. } => false,
            },
        }
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
        let mut path = vec!["organizations", &organization, "profiles", profile];
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
        /// The sealed Session this one carries on from, by generated name, identifier, any
        /// unambiguous prefix of its identifier, or `latest`
        #[arg(long, value_name = "SESSION")]
        continues: Option<String>,
    },
    /// List every Session in the Organization
    List,
    /// Show a Session
    Show {
        /// Its generated name, its identifier, any unambiguous prefix of its identifier, or `latest`
        session: String,
    },
    /// Add a participant's message; starts a Run or queues its next Turn
    Post {
        /// Its generated name, its identifier, any unambiguous prefix of its identifier, or `latest`
        session: String,
        /// The participant saying the message
        #[arg(long, default_value = "operator")]
        as_participant: String,
        /// What the participant says
        message: String,
    },
    /// Seal a Session: readable ever after, and never reopened
    Seal {
        /// Its generated name, its identifier, any unambiguous prefix of its identifier, or `latest`
        session: String,
    },
    /// Read a Session's Transcript, one JSON entry a line, and the cursor a later read
    /// resumes from
    Transcript {
        /// Its generated name, its identifier, any unambiguous prefix of its identifier, or `latest`
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
        /// The Session it executes on behalf of, by generated name, identifier, any
        /// unambiguous prefix of its identifier, or `latest`
        #[arg(long)]
        session: String,
        /// The model it works with, or none for its Agent's or Agent Runtime's default
        #[arg(long)]
        model: Option<String>,
    },
    /// List every Run in a Session
    List {
        /// The Session the Runs execute on behalf of, by generated name, identifier, any
        /// unambiguous prefix of its identifier, or `latest`
        #[arg(long)]
        session: String,
    },
    /// Show a Run
    Show {
        /// Its generated name, its identifier, any unambiguous prefix of its identifier, or `latest`
        run: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let matches = Client::command().get_matches();
    let client = Client::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    let control_plane: Url = client
        .control_plane
        .parse()
        .with_context(|| format!("{} is no control-plane URL", client.control_plane))?;
    let control_plane_source = match matches.value_source("control_plane") {
        Some(ValueSource::CommandLine) => "--control-plane",
        Some(ValueSource::EnvVariable) => CONTROL_PLANE_VARIABLE,
        _ => "default",
    };

    let api = ControlPlane::at(control_plane.clone());
    let presentation = Presentation::chosen(client.json.as_deref())?;

    let named = client.organization.map(|organization| Scope {
        organization,
        source: match matches.value_source("organization") {
            Some(ValueSource::EnvVariable) => Source::Environment,
            _ => Source::Flag,
        },
    });
    if named
        .as_ref()
        .is_some_and(|scope| matches!(scope.source, Source::Flag))
        && !client.command.scoped()
    {
        bail!("--organization scopes nothing here; this command names its record directly");
    }
    let scoping = Scoping::new(&api, named);

    match client.command {
        Command::Apply(Apply { file }) => {
            let organization = scoping.resolve().await?.organization;
            let declaration = declaration(&file)?;
            let preview = api
                .post(
                    &["organizations", &organization, "declaration", "preview"],
                    &declaration,
                )
                .await?;
            show_declaration(&preview)?;
            api.post(
                &["organizations", &organization, "declaration"],
                &declaration,
            )
            .await?;
        }
        Command::Organization(OrganizationCommand::Declare { name }) => {
            show(
                &presentation,
                &view::DECLARED,
                &api.post(&["organizations"], &json!({ "name": name }))
                    .await?,
            )?;
        }
        Command::Organization(OrganizationCommand::List) => {
            show(
                &presentation,
                &view::ORGANIZATIONS,
                &api.get(&["organizations"]).await?,
            )?;
        }
        Command::Workspace(WorkspaceCommand::Declare {
            name,
            repositories,
            branch,
        }) => {
            let organization = scoping.resolve().await?.organization;
            let declaration = json!({
                "name": name,
                "repositories": repositories,
                "branch": branch,
            });
            show(
                &presentation,
                &view::DECLARED,
                &api.post(
                    &["organizations", &organization, "workspaces"],
                    &declaration,
                )
                .await?,
            )?;
        }
        Command::Workspace(WorkspaceCommand::List) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::WORKSPACES,
                &api.get(&["organizations", &organization, "workspaces"])
                    .await?,
            )?;
        }
        Command::Agent(AgentCommand::Declare {
            name,
            runtime,
            model,
        }) => {
            let organization = scoping.resolve().await?.organization;
            let declaration = json!({
                "name": name,
                "runtime": runtime,
                "model": model,
            });
            show(
                &presentation,
                &view::DECLARED,
                &api.post(&["organizations", &organization, "agents"], &declaration)
                    .await?,
            )?;
        }
        Command::Agent(AgentCommand::List) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::AGENTS,
                &api.get(&["organizations", &organization, "agents"]).await?,
            )?;
        }
        Command::Credential(CredentialCommand::Set { variable }) => {
            let organization = scoping.resolve().await?.organization;
            let secret = json!({ "secret": read_the_secret()? });
            show(
                &presentation,
                &view::CREDENTIAL,
                &api.put(
                    &["organizations", &organization, "credentials", &variable],
                    &secret,
                )
                .await?,
            )?;
        }
        Command::Credential(CredentialCommand::List) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::CREDENTIALS,
                &api.get(&["organizations", &organization, "credentials"])
                    .await?,
            )?;
        }
        Command::Credential(CredentialCommand::Forget { variable }) => {
            let organization = scoping.resolve().await?.organization;
            api.delete(&["organizations", &organization, "credentials", &variable])
                .await?;
        }
        Command::Profile(ProfileCommand::Declare { name, owner }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::DECLARED,
                &api.post(
                    &["organizations", &organization, "profiles"],
                    &json!({ "name": name, "owner": owner }),
                )
                .await?,
            )?;
        }
        Command::Profile(ProfileCommand::Set { name, entry }) => {
            let organization = scoping.resolve().await?.organization;
            let login = json!({ "secret": read_the_login(entry.file.is_some())? });
            show(
                &presentation,
                &view::LOGIN,
                &api.put(&entry.path(&organization, &name), &login).await?,
            )?;
        }
        Command::Profile(ProfileCommand::List) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::PROFILES,
                &api.get(&["organizations", &organization, "profiles"])
                    .await?,
            )?;
        }
        Command::Profile(ProfileCommand::Forget { name, entry }) => {
            let organization = scoping.resolve().await?.organization;
            api.delete(&entry.path(&organization, &name)).await?;
        }
        Command::Integration(IntegrationCommand::Register(register)) => {
            let organization = scoping.resolve().await?.organization;
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
            show(
                &presentation,
                &view::DECLARED,
                &api.post(
                    &["organizations", &organization, "integrations"],
                    &registration,
                )
                .await?,
            )?;
        }
        Command::Integration(IntegrationCommand::List) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::INTEGRATIONS,
                &api.get(&["organizations", &organization, "integrations"])
                    .await?,
            )?;
        }
        Command::Integration(IntegrationCommand::AcknowledgeRefusal { name }) => {
            let organization = scoping.resolve().await?.organization;
            api.delete(&[
                "organizations",
                &organization,
                "integrations",
                &name,
                "event-refusal",
            ])
            .await?;
        }
        Command::Event(EventCommand::List { limit }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::EVENTS,
                &api.get_where(
                    &["organizations", &organization, "events"],
                    &[("limit", &limit.to_string())],
                )
                .await?,
            )?;
        }
        Command::Event(EventCommand::Show { record }) => {
            show(
                &presentation,
                &view::EVENT,
                &api.get(&["events", &record]).await?,
            )?;
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
            let organization = scoping.resolve().await?.organization;
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
            show(
                &presentation,
                &view::DECLARED,
                &api.post(&["organizations", &organization, "triggers"], &declaration)
                    .await?,
            )?;
        }
        Command::Trigger(TriggerCommand::List) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::TRIGGERS,
                &api.get(&["organizations", &organization, "triggers"])
                    .await?,
            )?;
        }
        Command::Trigger(TriggerCommand::Show { name }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::TRIGGER,
                &api.get(&["organizations", &organization, "triggers", &name])
                    .await?,
            )?;
        }
        Command::Trigger(TriggerCommand::Test {
            name,
            event,
            instruction,
        }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::TRIGGER_TEST,
                &api.post(
                    &["organizations", &organization, "triggers", &name, "test"],
                    &json!({ "event": event, "instruction": instruction }),
                )
                .await?,
            )?;
        }
        Command::Trigger(TriggerCommand::Disable { name }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::TRIGGER_STATE,
                &api.post(
                    &["organizations", &organization, "triggers", &name, "disable"],
                    &json!({}),
                )
                .await?,
            )?;
        }
        Command::Trigger(TriggerCommand::Enable { name }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::TRIGGER_STATE,
                &api.post(
                    &["organizations", &organization, "triggers", &name, "enable"],
                    &json!({}),
                )
                .await?,
            )?;
        }
        Command::Session(SessionCommand::Open {
            workspace,
            agent,
            profile,
            branch,
            continues,
        }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::DECLARED,
                &api.post(
                    &["organizations", &organization, "sessions"],
                    &json!({
                        "workspace": workspace,
                        "agent": agent,
                        "profile": profile,
                        "branch": branch,
                        "continues": continues,
                    }),
                )
                .await?,
            )?;
        }
        Command::Session(SessionCommand::List) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::SESSIONS,
                &api.get(&["organizations", &organization, "sessions"])
                    .await?,
            )?;
        }
        Command::Session(SessionCommand::Show { session }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::SESSION,
                &api.get(&["organizations", &organization, "sessions", &session])
                    .await?,
            )?;
        }
        Command::Session(SessionCommand::Post {
            session,
            as_participant,
            message,
        }) => {
            let organization = scoping.resolve().await?.organization;
            let answer = api
                .post(
                    &[
                        "organizations",
                        &organization,
                        "sessions",
                        &session,
                        "messages",
                    ],
                    &json!({ "participant": as_participant, "message": message }),
                )
                .await?;
            if answer.is_null() {
                eprintln!("queued as the next turn of the run already in flight");
            }
            show(&presentation, &view::DECLARED, &answer)?;
        }
        Command::Session(SessionCommand::Seal { session }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::DECLARED,
                &api.post(
                    &["organizations", &organization, "sessions", &session, "seal"],
                    &json!({}),
                )
                .await?,
            )?;
        }
        Command::Session(SessionCommand::Transcript {
            session,
            cursor,
            follow,
        }) => {
            let organization = scoping.resolve().await?.organization;
            let read = transcript::read(
                &control_plane,
                &organization,
                &session,
                cursor,
                follow,
                &presentation,
            )
            .await?;
            // Beside the Transcript rather than in it, so stdout carries entries and nothing else.
            if let Some(cursor) = read {
                eprintln!("cursor  {cursor}");
            }
        }
        Command::Run(RunCommand::Enqueue { session, model }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::DECLARED,
                &api.post(
                    &["organizations", &organization, "sessions", &session, "runs"],
                    &json!({ "model": model }),
                )
                .await?,
            )?;
        }
        Command::Run(RunCommand::List { session }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::RUNS,
                &api.get(&["organizations", &organization, "sessions", &session, "runs"])
                    .await?,
            )?;
        }
        Command::Run(RunCommand::Show { run }) => {
            let organization = scoping.resolve().await?.organization;
            show(
                &presentation,
                &view::RUN,
                &api.get(&["organizations", &organization, "runs", &run])
                    .await?,
            )?;
        }
        Command::Status => {
            let location = json!({
                "control_plane": client.control_plane,
                "control_plane_source": control_plane_source,
            });
            status(&api, &presentation, location, scoping.derive().await?).await?;
        }
    }

    Ok(())
}

#[derive(Deserialize)]
struct AppliedDeclaration {
    declarations: Vec<Declared>,
    admitting_outsiders: Vec<String>,
}

#[derive(Deserialize)]
struct Declared {
    kind: String,
    name: String,
    action: Action,
    differences: Vec<Difference>,
}

#[derive(Deserialize)]
struct Difference {
    field: String,
    was: Option<String>,
    becomes: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Add,
    Change,
    Unchanged,
}

fn declaration(file: &str) -> Result<Value> {
    let mut text = String::new();
    if file == "-" {
        std::io::stdin()
            .read_to_string(&mut text)
            .context("reading the declaration from standard input")?;
    } else {
        text = std::fs::read_to_string(file)
            .with_context(|| format!("reading the declaration file {file}"))?;
    }

    yaml_serde::from_str(&text).context("reading the declaration file")
}

fn show_declaration(value: &Value) -> Result<()> {
    let applied: AppliedDeclaration =
        serde_json::from_value(value.clone()).context("reading the declaration preview")?;
    for declared in applied.declarations {
        let sign = match declared.action {
            Action::Add => '+',
            Action::Change => '~',
            Action::Unchanged => '=',
        };
        println!("{sign} {} {}", declared.kind, declared.name);
        for difference in declared.differences {
            println!("    {}", difference.field);
            for (sign, value) in [('-', difference.was), ('+', difference.becomes)] {
                for line in value.iter().flat_map(|value| value.lines()) {
                    println!("      {sign} {line}");
                }
            }
        }
    }
    for name in applied.admitting_outsiders {
        warn_of_outsiders(&name);
    }

    Ok(())
}

fn warn_of_outsiders(name: &str) {
    eprintln!(
        "warning: the trigger {name} fires for events from people outside the organization. \
         Until 0.4, it is an unsupervised agent with your credentials on your repository, \
         briefed by whatever a stranger writes. Filter on the author_association GitHub \
         reports as OWNER, MEMBER or COLLABORATOR to decline strangers, or keep it as a \
         decision you made on purpose."
    );
}

async fn status(
    api: &ControlPlane,
    presentation: &Presentation,
    location: Value,
    derived: Derived,
) -> Result<()> {
    let scope = match derived {
        Derived::Scope(scope) => scope,
        Derived::Unnamed { existing } => {
            let next = if existing.is_empty() {
                format!("{BINARY} organization declare <name>")
            } else {
                format!("{BINARY} status --organization <name>")
            };
            show(
                presentation,
                &view::UNRESOLVED,
                &merged(
                    location,
                    json!({
                        "organization": null,
                        "organization_source": null,
                        "organizations": existing,
                        "next": next,
                    }),
                ),
            )?;
            return Ok(());
        }
    };

    let organization = scope.organization.as_str();
    let within = async |records| api.get(&["organizations", organization, records]).await;
    let (workspaces, agents, triggers, sessions, integrations, credentials, profiles) = tokio::try_join!(
        within("workspaces"),
        within("agents"),
        within("triggers"),
        within("sessions"),
        within("integrations"),
        within("credentials"),
        within("profiles"),
    )?;
    let workspace_names = names(&workspaces);
    let agent_names = names(&agents);

    show(
        presentation,
        &view::STATUS,
        &merged(
            location,
            json!({
                "organization": organization,
                "organization_source": scope.source.to_string(),
                "workspaces": count(&workspaces),
                "agents": count(&agents),
                "triggers": count(&triggers),
                "sessions": count(&sessions),
                "integrations": count(&integrations),
                "credentials": count(&credentials),
                "profiles": count(&profiles),
                "next": next_command(&workspace_names, &agent_names),
            }),
        ),
    )?;

    Ok(())
}

fn merged(mut record: Value, more: Value) -> Value {
    if let (Some(record), Value::Object(more)) = (record.as_object_mut(), more) {
        record.extend(more);
    }
    record
}

fn count(records: &Value) -> usize {
    records.as_array().map_or(0, Vec::len)
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
            format!("{BINARY} session open --workspace {workspace} --agent {agent}")
        }
        (Some(_), None) => format!("{BINARY} agent declare <name>"),
        (None, _) => {
            format!("{BINARY} workspace declare <name> --repository <url> --branch <branch>")
        }
    }
}

/// A file is held exactly as it was read, because it is written back exactly as it was held.
fn read_the_login(file: bool) -> Result<String> {
    let read = read_standard_input("a login")?;

    if read.trim().is_empty() {
        bail!("a login is read from standard input, and nothing was on it");
    }

    Ok(if file { read } else { read.trim().to_owned() })
}

/// Off standard input rather than out of an argument, so a provider's key is never in a shell
/// history or in what `ps` shows of this process.
fn read_the_secret() -> Result<String> {
    let secret = read_standard_input("a provider credential")?
        .trim()
        .to_owned();

    if secret.is_empty() {
        bail!("a provider credential is read from standard input, and nothing was on it");
    }

    Ok(secret)
}

/// The asking happens only where someone is there to read it: a pipe is read in silence, so
/// nothing driving the Client can block on a prompt it cannot see.
fn read_standard_input(what: &str) -> Result<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("reading {what} from standard input; end it with ctrl-d");
    }

    let mut read = String::new();
    stdin.read_to_string(&mut read)?;

    Ok(read)
}
