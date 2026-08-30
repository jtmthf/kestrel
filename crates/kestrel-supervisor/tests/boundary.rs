//! The courier carries no cargo (ADR-0002), as something you can fail a build on: the
//! supervisor forwards, blocks, relays and holds a cursor, and never reasons about the domain.

use std::fs;
use std::path::{Path, PathBuf};

const DOMAIN: [&str; 10] = [
    "session",
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

#[test]
fn nothing_in_the_supervisor_names_a_thing_only_the_control_plane_may_reason_about() {
    let sources = sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    assert!(!sources.is_empty(), "the supervisor should have sources");

    for source in sources {
        let spoken = fs::read_to_string(&source)
            .expect("a readable source file")
            .to_lowercase();

        for word in DOMAIN {
            assert!(
                !spoken.contains(word),
                "{} names {word}, which is the control plane's to know",
                source.display()
            );
        }
    }
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
