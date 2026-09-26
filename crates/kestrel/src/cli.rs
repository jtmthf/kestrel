use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand, ValueEnum};
use directories::ProjectDirs;

use crate::compute::{Docker, Driver, LocalExec};
use crate::role::serve::Listen;
use crate::role::work::{Dispatch, HarnessCommand};

const SUPERVISOR: &str = "kestrel-supervisor";
const IMAGE: &str = "kestrel-env:latest";
const DEFAULT_MAX_ACTIVE_RUNS: NonZeroUsize = NonZeroUsize::new(2).unwrap();

const ROLES: &str = "\
Roles:
  The control plane runs as one of two roles, selected by argv on one image: `serve` and
  `work`. Operators reach it with the `kestrel` Client, a separate program.

  Run it with no command to start every role in one process. That is the default.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Serve,
    Work,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Serve => "serve",
            Role::Work => "work",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "kestrel-control-plane",
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

    /// Where the control plane listens for the link a supervisor dials out to, and for webhooks
    #[arg(
        long,
        env = "KESTREL_LISTEN",
        global = true,
        value_name = "ADDR",
        default_value = "127.0.0.1:7717"
    )]
    pub listen: SocketAddr,

    /// Where the control plane listens for Clients; it authenticates nobody, so keep it on
    /// loopback and reach a remote one through a tunnel
    #[arg(
        long,
        env = "KESTREL_OPERATOR_LISTEN",
        global = true,
        value_name = "ADDR",
        default_value = "127.0.0.1:7718"
    )]
    operator_listen: SocketAddr,

    /// Where a supervisor reaches the link, if not the address the control plane bound
    #[arg(long, env = "KESTREL_LINK", global = true, value_name = "URL")]
    link: Option<String>,

    /// The supervisor each Run starts on its Instance, if not the one beside this binary
    #[arg(long, env = "KESTREL_SUPERVISOR", global = true, value_name = "PATH")]
    supervisor: Option<PathBuf>,

    /// The command a supervisor spawns for each Harness an Agent may name, as
    /// NAME=COMMAND; repeat, or separate with commas, for many
    #[arg(
        long = "harness-command",
        env = "KESTREL_HARNESS_COMMANDS",
        global = true,
        value_name = "NAME=COMMAND",
        value_delimiter = ',',
        default_value = "opencode=opencode acp --print-logs,claude=claude-agent-acp,codex=codex-acp"
    )]
    harnesses: Vec<HarnessCommand>,

    /// The ACP authentication method a Harness is logged in with, for one that requires
    /// being logged in before it will open a workspace
    #[arg(long, env = "KESTREL_AGENT_AUTH", global = true, value_name = "METHOD")]
    agent_auth: Option<String>,

    /// The Compute driver a Workspace's Instance is provisioned by
    #[arg(
        long = "compute",
        env = "KESTREL_COMPUTE",
        global = true,
        value_name = "DRIVER",
        default_value = "docker"
    )]
    compute: ComputeDriver,

    /// The image the Docker driver provisions an Instance from
    #[arg(
        long,
        env = "KESTREL_IMAGE",
        global = true,
        value_name = "IMAGE",
        default_value = IMAGE
    )]
    image: String,

    /// The network an Instance joins, if not the daemon's default
    #[arg(long, env = "KESTREL_NETWORK", global = true, value_name = "NETWORK")]
    network: Option<String>,

    /// Runs getting to or mid-turn at once; excess work waits, and zero would never make progress
    #[arg(
        long,
        env = "KESTREL_MAX_ACTIVE_RUNS",
        global = true,
        value_name = "RUNS",
        default_value_t = DEFAULT_MAX_ACTIVE_RUNS
    )]
    max_active_runs: NonZeroUsize,

    /// A Harness whose Runs on one Subscription Profile are dispatched one at a time,
    /// because the login they share rotates as it refreshes; repeat, or separate with commas
    #[arg(
        long = "serialized-harness",
        env = "KESTREL_SERIALIZED_HARNESS",
        global = true,
        value_name = "NAME",
        value_delimiter = ',',
        default_value = "codex"
    )]
    serialized_harnesses: Vec<String>,
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
    /// Serve the link a supervisor dials out to, the webhooks Events arrive by, and the
    /// operator boundary Clients reach
    Serve,
    /// Claim queued Runs and execute them
    Work,
}

impl Cli {
    pub fn listen(&self) -> Listen {
        Listen {
            link: self.listen,
            operator: self.operator_listen,
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
            harnesses: self.harnesses.clone(),
            auth: self.agent_auth.clone(),
            max_active_runs: self.max_active_runs,
            serialized: self
                .serialized_harnesses
                .iter()
                .filter(|harness| !harness.is_empty())
                .cloned()
                .collect(),
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
        let mut args = vec!["kestrel-control-plane"];
        args.extend_from_slice(argv);
        Cli::parse_from(args)
    }

    #[test]
    fn no_role_selects_every_role_in_one_process() {
        assert_eq!(parsed(&[]).command, None);
    }

    #[test]
    fn serve_selects_the_serve_role() {
        assert_eq!(parsed(&["serve"]).command, Some(Command::Serve));
    }

    #[test]
    fn work_selects_the_work_role() {
        assert_eq!(parsed(&["work"]).command, Some(Command::Work));
    }

    #[test]
    fn an_operator_command_is_no_role_of_the_control_plane() {
        assert!(Cli::try_parse_from(["kestrel-control-plane", "organization", "list"]).is_err());
    }

    #[test]
    fn an_unknown_command_is_rejected_rather_than_run_as_a_role() {
        assert!(Cli::try_parse_from(["kestrel-control-plane", "wrok"]).is_err());
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

    fn spawned(dispatch: &Dispatch) -> Vec<(&str, &str)> {
        dispatch
            .harnesses
            .iter()
            .map(|harness| (harness.name.as_str(), harness.command.as_str()))
            .collect()
    }

    #[test]
    fn every_harness_the_development_image_carries_is_spawned_unless_configuration_says_otherwise()
    {
        assert_eq!(
            spawned(&dispatch(&[])),
            [
                ("opencode", "opencode acp --print-logs"),
                ("claude", "claude-agent-acp"),
                ("codex", "codex-acp"),
            ]
        );
        assert_eq!(
            spawned(&dispatch(&[
                "--harness-command",
                "opencode=opencode acp --log-level debug",
                "--harness-command",
                "codex=codex-acp,claude=claude-agent-acp"
            ])),
            [
                ("opencode", "opencode acp --log-level debug"),
                ("codex", "codex-acp"),
                ("claude", "claude-agent-acp"),
            ]
        );
    }

    #[test]
    fn a_harness_named_without_its_command_is_rejected() {
        for given in ["opencode", "=opencode acp", "opencode="] {
            assert!(
                Cli::try_parse_from(["kestrel-control-plane", "--harness-command", given]).is_err(),
                "{given} was accepted"
            );
        }
    }

    #[test]
    fn a_driver_that_is_neither_is_rejected_rather_than_falling_back() {
        assert!(
            Cli::try_parse_from(["kestrel-control-plane", "--compute", "firecracker"]).is_err()
        );
    }

    #[test]
    fn two_runs_may_be_active_unless_configuration_says_otherwise() {
        assert_eq!(dispatch(&[]).max_active_runs.get(), 2);
        assert_eq!(
            dispatch(&["--max-active-runs", "5"]).max_active_runs.get(),
            5
        );
    }

    #[test]
    fn an_active_run_limit_of_zero_is_rejected() {
        assert!(Cli::try_parse_from(["kestrel-control-plane", "--max-active-runs", "0"]).is_err());
    }

    #[test]
    fn the_operator_boundary_listens_on_loopback_unless_configuration_says_otherwise() {
        assert!(parsed(&[]).listen().operator.ip().is_loopback());
    }

    #[test]
    fn the_operator_boundary_and_the_link_are_configured_apart() {
        let listen = parsed(&[
            "--listen",
            "0.0.0.0:7717",
            "--operator-listen",
            "127.0.0.1:9000",
        ])
        .listen();

        assert_eq!(listen.link, "0.0.0.0:7717".parse().expect("an address"));
        assert_eq!(
            listen.operator,
            "127.0.0.1:9000".parse().expect("an address")
        );
    }

    #[test]
    fn help_lists_the_two_roles() {
        let help = rendered_help();
        let spoken = help.to_lowercase();
        for role in [Role::Serve, Role::Work] {
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
