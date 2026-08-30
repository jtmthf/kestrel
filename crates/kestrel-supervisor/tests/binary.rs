//! What the supervisor does once it has a link to dial is proved against a real control plane
//! by the primary test seam, in the kestrel crate.

use std::process::Command;

#[test]
fn the_supervisor_binary_starts_and_says_it_has_no_link_to_dial() {
    let supervisor = Command::new(env!("CARGO_BIN_EXE_kestrel-supervisor"))
        .env_clear()
        .output()
        .expect("the supervisor should spawn");

    let said = String::from_utf8_lossy(&supervisor.stderr);

    assert_eq!(
        supervisor.status.code(),
        Some(1),
        "the supervisor exited {}. it said:\n{said}",
        supervisor.status
    );
    assert!(said.contains("supervisor started"), "it said:\n{said}");
    assert!(said.contains("no link to dial"), "it said:\n{said}");
}
