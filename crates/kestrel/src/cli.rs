use clap::{Parser, Subcommand};

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
pub enum Selection {
    AllInOne,
    Serve,
    Work,
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
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Serve the API and the link an Environment dials out to
    Serve,
    /// Claim queued Runs and execute them
    Work,
}

impl Cli {
    /// Every other command is the CLI role; this match being exhaustive is what forces the
    /// first one to say so.
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
        assert!(Cli::try_parse_from(["kestrel", "wrok"]).is_err());
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
