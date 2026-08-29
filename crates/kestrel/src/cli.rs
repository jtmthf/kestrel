//! argv, and the role it selects.
//!
//! kestrel is one binary. Which of the three roles a process runs as is selected by argv
//! rather than by a different artifact, so the deployment shapes that split the roles later
//! do not need a second image (ADR-0002).

use clap::{Parser, Subcommand};

/// Named in `--help` after the command list, because the CLI role is every command that is
/// not `serve` or `work` and so cannot be listed as one entry alongside them.
const ROLES: &str = "\
Roles:
  kestrel runs as one of three roles, selected by argv on one image: `serve`, `work`, and
  the CLI — every other command, which does its one thing and exits.

  Run kestrel with no command to start every role in one process. That is the default,
  and at 0.1 it is the only supported topology.";

/// One of the three roles a `kestrel` process can run as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Serve,
    Work,
    Cli,
}

impl Role {
    /// The name this role is known by in argv and in a log line.
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

/// Which long-running role, or roles, argv selected.
///
/// Kept separate from [`Role`], rather than folded into an `Only(Role)` wrapper around it,
/// because the CLI role is one-shot: it is not something a process is *started as* and then
/// waits in, so it has no place here. The first CLI command lands in 0.1/02 and brings its
/// own variant with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Every long-running role in one process — the default.
    AllInOne,
    /// The `serve` role alone.
    Serve,
    /// The `work` role alone.
    Work,
}

#[derive(Debug, Parser)]
#[command(
    name = "kestrel",
    version,
    about = "kestrel — background agents, triggered by the events a team already produces.",
    // So the command list names roles and nothing else; `--help` already does this job.
    disable_help_subcommand = true,
    after_help = ROLES,
    after_long_help = ROLES
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Serve the API and the link an Environment dials out to
    Serve,
    /// Claim queued Runs and execute them
    Work,
}

impl Cli {
    /// What argv asked this process to be.
    ///
    /// `serve` and `work` name the two long-running roles; every other command is the CLI
    /// role, and there are none yet. Because this match is exhaustive, the first one cannot
    /// land without being given a role here.
    pub fn selection(&self) -> Selection {
        match &self.command {
            None => Selection::AllInOne,
            Some(Command::Serve) => Selection::Serve,
            Some(Command::Work) => Selection::Work,
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

    fn selection_from(argv: &[&str]) -> Selection {
        let mut args = vec!["kestrel"];
        args.extend_from_slice(argv);
        Cli::parse_from(args).selection()
    }

    #[test]
    fn no_role_selects_every_role_in_one_process() {
        assert_eq!(selection_from(&[]), Selection::AllInOne);
    }

    #[test]
    fn serve_selects_the_serve_role() {
        assert_eq!(selection_from(&["serve"]), Selection::Serve);
    }

    #[test]
    fn work_selects_the_work_role() {
        assert_eq!(selection_from(&["work"]), Selection::Work);
    }

    #[test]
    fn an_unknown_command_is_rejected_rather_than_run_as_a_role() {
        let mut args = vec!["kestrel"];
        args.push("wrok");
        assert!(Cli::try_parse_from(args).is_err());
    }

    #[test]
    fn help_lists_the_three_roles() {
        let help = rendered_help();
        // Case-insensitively: help prose says "the CLI", argv says `cli`.
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
