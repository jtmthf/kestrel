//! The scripts the agent plays, shared with the harness that selects one.

use clap::ValueEnum;

/// The two models the agent offers a client, the first of which it runs on unasked.
pub const DEFAULT_MODEL: &str = "scripted-mini";
pub const OTHER_MODEL: &str = "scripted-max";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Script {
    /// Plans, thinks, calls a tool it asks permission for, says two messages and ends the turn.
    Speaks,
    /// Says one thing and ends the turn without having finished.
    Refuses,
    /// Dies mid-turn without answering the prompt.
    Dies,
    /// Answers `initialize` with a protocol version it was not asked for.
    Predates,
    /// Answers `initialize` offering only a terminal to log in at.
    Demands,
    /// Offers a client no say in which model it runs on.
    Decides,
    /// Will not open a session for a client that has not logged in.
    Insists,
    /// Works at a turn that never ends, so nothing the agent does is what ends the Run.
    Dawdles,
    /// Speaks, but takes long enough over the turn that the control plane can be killed and
    /// restarted while the Run is still in flight.
    Lingers,
}

impl Script {
    pub const fn as_str(self) -> &'static str {
        match self {
            Script::Speaks => "speaks",
            Script::Refuses => "refuses",
            Script::Dies => "dies",
            Script::Predates => "predates",
            Script::Demands => "demands",
            Script::Decides => "decides",
            Script::Insists => "insists",
            Script::Dawdles => "dawdles",
            Script::Lingers => "lingers",
        }
    }
}
