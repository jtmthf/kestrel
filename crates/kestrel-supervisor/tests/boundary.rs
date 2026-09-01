//! The courier carries no cargo (ADR-0002), as something you can fail a build on: the
//! supervisor forwards, blocks, relays and holds a cursor, and never reasons about the domain.

use std::fs;
use std::path::{Path, PathBuf};

const DOMAIN: [&str; 9] = [
    "workspace",
    "organization",
    "transcript",
    "campaign",
    "trigger",
    "workflow",
    "approval",
    "policy",
    "audit",
];

/// Caught as a whole name rather than anywhere in one, because ACP's own `SessionUpdate`,
/// `session_id` and `session/prompt` carry it and CONTEXT.md gives ACP's session no meaning of
/// kestrel's.
const ALSO_THE_WIRE_S: [&str; 1] = ["session"];

#[test]
fn nothing_in_the_supervisor_names_a_thing_only_the_control_plane_may_reason_about() {
    let sources = sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    assert!(!sources.is_empty(), "the supervisor should have sources");

    for source in sources {
        let spoken = spoken(&source);

        for word in DOMAIN {
            assert!(
                !spoken.contains(word),
                "{} names {word}, which is the control plane's to know",
                source.display()
            );
        }
        for word in ALSO_THE_WIRE_S {
            assert!(
                !names(&spoken).any(|name| name == word),
                "{} names {word} on its own, which is the control plane's to know",
                source.display()
            );
        }
    }
}

/// kestrel implements someone else's client (ADR-0007), so which agent is on the other end of
/// it is not something the supervisor may look at.
#[test]
fn nothing_in_the_supervisor_names_an_agent_it_might_be_driving() {
    for source in sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src")) {
        let spoken = spoken(&source);

        for agent in ["opencode", "claude", "codex", "gemini"] {
            assert!(
                !spoken.contains(agent),
                "{} names {agent}, and kestrel drives whatever speaks ACP",
                source.display()
            );
        }
    }
}

fn spoken(source: &Path) -> String {
    fs::read_to_string(source)
        .expect("a readable source file")
        .to_lowercase()
}

/// A `/` holds a name together, so an ACP method is one name rather than two.
fn names(spoken: &str) -> impl Iterator<Item = &str> {
    spoken
        .split(|character: char| !character.is_alphanumeric() && !"_/".contains(character))
        .filter(|name| !name.is_empty())
}

fn sources(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .expect("a readable source directory")
        .flat_map(|entry| {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                sources(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}
