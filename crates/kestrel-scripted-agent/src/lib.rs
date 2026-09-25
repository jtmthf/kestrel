//! The scripts the agent plays, shared with the harness that selects one.

use clap::ValueEnum;

/// The prefix of every environment variable the `Confides` script says it can see. No provider
/// names one this way, so a real key in the environment the suite runs in cannot be said by
/// accident.
pub const CONFIDED: &str = "SCRIPTED_";

/// Where the `Refreshes` script keeps its login, beneath its home.
pub const LOGIN: &str = ".scripted/login";
/// What the `Refreshes` script appends to the login it found, standing in for a new token.
pub const REFRESHED: &str = " refreshed";

/// The two models the agent offers a client, the first of which it runs on unasked.
pub const DEFAULT_MODEL: &str = "scripted-mini";
pub const OTHER_MODEL: &str = "scripted-max";
/// What the `Mutters` script writes to stderr, followed by a line of `OVERLONG` bytes.
pub const MUTTERED: &str = "the scripted agent muttered to itself";
pub const OVERLONG: usize = 64 * 1024;
pub const FIRST_MEMORY: &str = "the first remembered message";
pub const LAST_MEMORY: &str = "the last remembered message";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Script {
    /// Plans, thinks, calls a tool it asks permission for, says two messages and ends the turn.
    Speaks,
    /// Says one thing and ends the turn without having finished.
    Refuses,
    /// Says which Provider Credentials reached its own process, and nothing else.
    Confides,
    /// Says the login it found beneath its home, and rewrites it there as a runtime refreshing
    /// one does.
    Refreshes,
    /// Says whether both ends of a long earlier context reached its prompt.
    Recalls,
    /// Says back exactly the prompt it was sent.
    Echoes,
    /// Says which turn of its one session each prompt is, and every prompt that came before it
    /// there, which only a conversation that went on can know.
    Converses,
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
    /// Writes to stderr as it works at a turn that never ends.
    Mutters,
    /// Speaks, but takes long enough over the turn that the control plane can be killed and
    /// restarted while the Run is still in flight.
    Lingers,
    /// Says the directory its session was opened against, and nothing else.
    Locates,
    /// Ends the turn without a message, a thought, a plan or a tool call, having sent only the
    /// bookkeeping a runtime sends when its agent says nothing.
    Silent,
    /// Calls a tool and ends the turn, without saying anything.
    Works,
    /// Asks permission and ends the turn, without a message, a thought, a plan or a tool call
    /// notification of its own.
    Asks,
    /// Answers its first turn, then produces nothing on the turns after it.
    Lapses,
    /// Converses, keeping its session on disk where a new process can load it, and dies the first
    /// time it is prompted for a second turn.
    Revives,
    /// Answers its turn, then exits between turns with nothing asked of it.
    Vanishes,
}

impl Script {
    pub const fn as_str(self) -> &'static str {
        match self {
            Script::Speaks => "speaks",
            Script::Refuses => "refuses",
            Script::Confides => "confides",
            Script::Refreshes => "refreshes",
            Script::Recalls => "recalls",
            Script::Echoes => "echoes",
            Script::Converses => "converses",
            Script::Dies => "dies",
            Script::Predates => "predates",
            Script::Demands => "demands",
            Script::Decides => "decides",
            Script::Insists => "insists",
            Script::Dawdles => "dawdles",
            Script::Mutters => "mutters",
            Script::Lingers => "lingers",
            Script::Locates => "locates",
            Script::Silent => "silent",
            Script::Works => "works",
            Script::Asks => "asks",
            Script::Lapses => "lapses",
            Script::Revives => "revives",
            Script::Vanishes => "vanishes",
        }
    }
}

/// What the `Converses` script says to the `turn`th prompt of its session, after `earlier`.
pub fn conversed(turn: usize, earlier: &[String]) -> String {
    format!("turn {turn}, after: {}", earlier.join(" | "))
}
