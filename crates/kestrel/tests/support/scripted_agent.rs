//! The stand-in Agent Runtime the main suite runs on, selected by naming a script in the
//! command an Environment spawns.

use std::path::PathBuf;
use std::sync::OnceLock;

pub use kestrel_scripted_agent::Script;

use super::built;

pub fn playing(script: Script) -> String {
    format!("\"{}\" --script {}", binary().display(), script.as_str())
}

fn binary() -> &'static PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();

    BINARY.get_or_init(|| built::binary("kestrel-scripted-agent"))
}
