//! Diagnostics for whoever is operating kestrel.
//!
//! Not the `Log` port. `Log` is a Session's Transcript — the record of what happened
//! between a Session's Participants (CONTEXT.md) — and nothing here writes to it.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// Sends diagnostics to stderr, so stdout stays free for what the CLI role prints.
///
/// Verbosity comes from `RUST_LOG`, defaulting to `info`. Colour is used only when stderr
/// is a terminal: everywhere kestrel actually runs, stderr is a pipe into a log collector,
/// and escape codes there are noise in the record rather than emphasis.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}
