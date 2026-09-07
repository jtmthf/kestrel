use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand, ValueEnum};
use directories::ProjectDirs;
use jiff::SignedDuration;

use crate::compute::{Docker, Driver, LocalExec};
use crate::domain::{Direction, SessionId};
use crate::integration::github;
use crate::log::Cursor;
use crate::role::work::Dispatch;

const SUPERVISOR: &str = "kestrel-supervisor";
const IMAGE: &str = "kestrel-env:latest";

const ROLES: &str = "\
Roles:
  kestrel runs as one of three roles, selected by argv on one image: `serve`, `work`, and
  the CLI — every other command, which does its one thing and exits.

  Run kestrel with no command to start every role in one process. That is the default,
  and at 0.1 it is the only supported topology.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Serve,
    Work,
    Cli,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Serve => "serve",
            Role::Work => "work",
            Role::Cli => "cli",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Not a wrapper around [`Role`]: the CLI role is one-shot, never started and waited in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection<'a> {
    AllInOne,
    Serve,
    Work,
    Cli(&'a CliCommand),
}

#[derive(Debug, Parser)]
#[command(
    name = "kestrel",
    version,
    about = "kestrel — background agents, triggered by the events a team already produces.",
    disable_help_subcommand = true,
    after_help = ROLES,
    after_long_help = ROLES
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Where kestrel keeps its database
    #[arg(long, env = "KESTREL_DATA_DIR", global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Where the control plane listens for the link an Environment dials out to
    #[arg(
        long,
        env = "KESTREL_LISTEN",
        global = true,
        value_name = "ADDR",
        default_value = "127.0.0.1:7717"
    )]
    pub listen: SocketAddr,

    /// Where an Environment reaches the link, if not the address the control plane bound
    #[arg(long, env = "KESTREL_LINK", global = true, value_name = "URL")]
    link: Option<String>,

    /// The supervisor an Environment runs, if not the one beside this binary
    #[arg(long, env = "KESTREL_SUPERVISOR", global = true, value_name = "PATH")]
    supervisor: Option<PathBuf>,

    /// The command an Environment spawns as its Agent Runtime and speaks ACP to
    #[arg(
        long,
        env = "KESTREL_AGENT_RUNTIME",
        global = true,
        value_name = "COMMAND",
        default_value = "opencode acp"
    )]
    agent_runtime: String,

    /// The ACP authentication method an Agent Runtime is logged in with, for one that requires
    /// being logged in before it will open a session
    #[arg(long, env = "KESTREL_AGENT_AUTH", global = true, value_name = "METHOD")]
    agent_auth: Option<String>,

    /// The Compute driver a Run's Environment is provisioned by
    #[arg(
        long = "compute",
        env = "KESTREL_COMPUTE",
        global = true,
        value_name = "DRIVER",
        default_value = "docker"
    )]
    compute: ComputeDriver,

    /// The image the Docker driver provisions an Environment from
    #[arg(
        long,
        env = "KESTREL_IMAGE",
        global = true,
        value_name = "IMAGE",
        default_value = IMAGE
    )]
    image: String,

    /// The network an Environment joins, if not the daemon's default
    #[arg(long, env = "KESTREL_NETWORK", global = true, value_name = "NETWORK")]
    network: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ComputeDriver {
    /// A container the Docker daemon on this machine runs
    Docker,
    /// A process tree on this machine
    LocalExec,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Serve the API and the link an Environment dials out to
    Serve,
    /// Claim queued Runs and execute them
    Work,
    #[command(flatten)]
    Cli(CliCommand),
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum CliCommand {
    /// Declare and list Organizations
    #[command(subcommand)]
    Organization(OrganizationCommand),
    /// Declare and list Workspaces
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Declare and list Agents
    #[command(subcommand)]
    Agent(AgentCommand),
    /// Hold and forget the Provider Credentials an Organization's Runs reach a model with
    #[command(subcommand)]
    Credential(CredentialCommand),
    /// Open and read Sessions
    #[command(subcommand)]
    Session(SessionCommand),
    /// Enqueue and list Runs
    #[command(subcommand)]
    Run(RunCommand),
    /// Register and list Integrations
    #[command(subcommand)]
    Integration(IntegrationCommand),
    /// Declare, inspect and disable Triggers
    #[command(subcommand)]
    Trigger(TriggerCommand),
    /// List the Events an Integration has discovered
    #[command(subcommand)]
    Event(EventCommand),
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum TriggerCommand {
    /// Declare a Trigger: what it matches, and the Agent and Workspace it starts work with
    Declare {
        /// The name it is referred to by
        name: String,
        /// The Organization it belongs to
        #[arg(long)]
        organization: String,
        /// The repository whose Events it matches, as owner/name
        #[arg(long, value_name = "OWNER/NAME")]
        repository: String,
        /// The label an issue is labelled with for it to fire
        #[arg(long)]
        label: String,
        /// The Workspace a firing's work happens against
        #[arg(long)]
        workspace: String,
        /// The Agent a firing starts work with
        #[arg(long)]
        agent: String,
    },
    /// List every Trigger in an Organization, and what each matches
    List {
        #[arg(long)]
        organization: String,
    },
    /// Show a Trigger
    Show {
        /// The name it is referred to by
        name: String,
        #[arg(long)]
        organization: String,
    },
    /// Stop a Trigger firing, without forgetting it
    Disable {
        /// The name it is referred to by
        name: String,
        #[arg(long)]
        organization: String,
    },
    /// Let a disabled Trigger fire again
    Enable {
        /// The name it is referred to by
        name: String,
        #[arg(long)]
        organization: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum IntegrationCommand {
    /// Register an Integration: a credentialed connection to an external system
    #[command(subcommand)]
    Register(RegisterCommand),
    /// List every Integration in an Organization, and the directions each carries
    List {
        #[arg(long)]
        organization: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum RegisterCommand {
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
        #[arg(long, env = "KESTREL_GITHUB_TOKEN", value_name = "TOKEN")]
        token: String,
        /// A direction it carries — inbound, outbound; repeat for both
        #[arg(
            long = "carries",
            value_name = "DIRECTION",
            default_values = ["inbound", "outbound"]
        )]
        carries: Vec<Direction>,
        /// How often the poll asks GitHub what has happened
        #[arg(long, value_name = "DURATION", default_value = "1m")]
        interval: SignedDuration,
        /// The API it reaches GitHub at, for an installation that is not github.com
        #[arg(long, env = "KESTREL_GITHUB_API", value_name = "URL", default_value = github::API)]
        api: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum EventCommand {
    /// List the Events recorded for an Organization, most recent first
    List {
        #[arg(long)]
        organization: String,
        /// How many to list at most
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum RunCommand {
    /// Enqueue a Run in a Session, for the work role to claim and dispatch
    Enqueue {
        /// The Session it executes on behalf of
        #[arg(long)]
        session: SessionId,
    },
    /// List every Run in a Session, with the Environment it executed in
    List {
        #[arg(long)]
        session: SessionId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SessionCommand {
    /// Open a Session against a Workspace and an Agent
    Open {
        /// The Organization it belongs to
        #[arg(long)]
        organization: String,
        /// The Workspace its work happens against
        #[arg(long)]
        workspace: String,
        /// The Agent that participates in it
        #[arg(long)]
        agent: String,
        /// The sealed Session this one carries on from
        #[arg(long, value_name = "SESSION")]
        continues: Option<SessionId>,
    },
    /// List every Session in an Organization, with the Event that started each
    List {
        #[arg(long)]
        organization: String,
    },
    /// Seal a Session: readable ever after, and never reopened
    Seal {
        /// The Session's identifier
        session: SessionId,
    },
    /// Add a participant's message; work starts now or after the active Run ends
    Post {
        session: SessionId,
        #[arg(long, default_value = "operator")]
        as_participant: String,
        message: String,
    },
    /// Show a Session
    Show {
        /// The Session's identifier
        session: SessionId,
    },
    /// Read one window of a Session's Transcript, and the cursor the next one resumes from
    Transcript {
        /// The Session's identifier
        session: SessionId,
        /// Resume from the cursor a previous read ended with
        #[arg(long)]
        cursor: Option<Cursor>,
        /// How many entries to read at most
        #[arg(long, value_name = "ENTRIES")]
        window: Option<usize>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum CredentialCommand {
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

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum AgentCommand {
    /// Declare an Agent: the actor identity that participates in a Session
    Declare {
        /// The name it is referred to by
        name: String,
        /// The Organization it belongs to
        #[arg(long)]
        organization: String,
        /// The Agent Runtime that drives it
        #[arg(long, default_value = "opencode")]
        runtime: String,
        /// The model it works with, or none for whatever its Agent Runtime defaults to
        #[arg(long)]
        model: Option<String>,
    },
    /// Change the model an Agent works with, leaving every Run in flight on the one it has
    Model {
        /// The name it is referred to by
        name: String,
        /// The Organization it belongs to
        #[arg(long)]
        organization: String,
        /// The model it works with, or none for whatever its Agent Runtime defaults to
        #[arg(long)]
        model: Option<String>,
    },
    /// List every Agent in an Organization
    List {
        #[arg(long)]
        organization: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum WorkspaceCommand {
    /// Declare a Workspace: the repositories and branch a Session's work happens against
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
    /// List every Workspace in an Organization
    List {
        #[arg(long)]
        organization: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum OrganizationCommand {
    /// Declare an Organization
    Declare {
        /// The name it is referred to by
        name: String,
    },
    /// List every Organization
    List,
}

impl Cli {
    /// Every other command is the CLI role; this match being exhaustive is what forces the
    /// first one to say so.
    pub fn selection(&self) -> Selection<'_> {
        match &self.command {
            None => Selection::AllInOne,
            Some(Command::Serve) => Selection::Serve,
            Some(Command::Work) => Selection::Work,
            Some(Command::Cli(command)) => Selection::Cli(command),
        }
    }

    /// The one choice between the two `Compute` drivers, made here from configuration so that
    /// nothing that executes a Run has to make it.
    pub fn dispatch(&self, bound: SocketAddr) -> Result<Dispatch> {
        Ok(Dispatch {
            link: self
                .link
                .clone()
                .unwrap_or_else(|| format!("http://{bound}")),
            driver: match self.compute {
                ComputeDriver::Docker => {
                    let docker = Docker::provisioning_from(&self.image);
                    Driver::Docker(match &self.network {
                        Some(network) => docker.on_network(network),
                        None => docker,
                    })
                }
                ComputeDriver::LocalExec => {
                    Driver::LocalExec(LocalExec::running(self.supervisor()?))
                }
            },
            runtime: self.agent_runtime.clone(),
            auth: self.agent_auth.clone(),
        })
    }

    fn supervisor(&self) -> Result<PathBuf> {
        if let Some(supervisor) = &self.supervisor {
            return Ok(supervisor.clone());
        }

        let beside = std::env::current_exe()
            .context("no path to this binary to find the supervisor beside")?
            .with_file_name(SUPERVISOR);

        Ok(if beside.exists() {
            beside
        } else {
            PathBuf::from(SUPERVISOR)
        })
    }

    pub fn data_dir(&self) -> Result<PathBuf> {
        match &self.data_dir {
            Some(dir) => Ok(dir.clone()),
            None => ProjectDirs::from("", "", "kestrel")
                .map(|dirs| dirs.data_dir().to_owned())
                .context("no home directory to keep kestrel's data in; pass --data-dir"),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    fn rendered_help() -> String {
        Cli::command().render_long_help().to_string()
    }

    fn parsed(argv: &[&str]) -> Cli {
        let mut args = vec!["kestrel"];
        args.extend_from_slice(argv);
        Cli::parse_from(args)
    }

    #[test]
    fn no_role_selects_every_role_in_one_process() {
        assert_eq!(parsed(&[]).selection(), Selection::AllInOne);
    }

    #[test]
    fn serve_selects_the_serve_role() {
        assert_eq!(parsed(&["serve"]).selection(), Selection::Serve);
    }

    #[test]
    fn work_selects_the_work_role() {
        assert_eq!(parsed(&["work"]).selection(), Selection::Work);
    }

    #[test]
    fn every_other_command_is_the_one_shot_cli_role() {
        assert_eq!(
            parsed(&["organization", "list"]).selection(),
            Selection::Cli(&CliCommand::Organization(OrganizationCommand::List))
        );
    }

    #[test]
    fn an_unknown_command_is_rejected_rather_than_run_as_a_role() {
        assert!(Cli::try_parse_from(["kestrel", "wrok"]).is_err());
    }

    fn dispatch(argv: &[&str]) -> Dispatch {
        parsed(argv)
            .dispatch("127.0.0.1:7717".parse().expect("an address"))
            .expect("the dispatch should build")
    }

    #[test]
    fn an_environment_is_a_container_unless_configuration_says_otherwise() {
        assert!(matches!(dispatch(&[]).driver, Driver::Docker(_)));
    }

    #[test]
    fn the_other_driver_is_reached_by_configuration_rather_than_by_a_different_command() {
        assert!(matches!(
            dispatch(&["--compute", "local-exec"]).driver,
            Driver::LocalExec(_)
        ));
        assert!(matches!(
            dispatch(&["--compute", "local-exec", "work"]).driver,
            Driver::LocalExec(_)
        ));
    }

    #[test]
    fn a_driver_that_is_neither_is_rejected_rather_than_falling_back() {
        assert!(Cli::try_parse_from(["kestrel", "--compute", "firecracker"]).is_err());
    }

    #[test]
    fn help_lists_the_three_roles() {
        let help = rendered_help();
        let spoken = help.to_lowercase();
        for role in [Role::Serve, Role::Work, Role::Cli] {
            assert!(
                spoken.contains(role.as_str()),
                "--help does not mention the {role} role:\n{help}"
            );
        }
    }

    #[test]
    fn help_names_the_all_in_one_default() {
        let help = rendered_help();
        assert!(
            help.contains("start every role in one process") && help.contains("the default"),
            "--help does not name the all-in-one default:\n{help}"
        );
    }
}
