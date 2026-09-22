//! The `kestrel` control-plane image: one artifact, every role selected by argv, over the
//! volume its database lives on, and reached by a Client that is not in it.
//!
//! Every test here builds and runs the image, which a `cargo test` has no business doing on
//! its own, so they are ignored by default and CI runs them with `--ignored`.

mod support;

use serde_json::Value;
use support::client;
use support::control_plane::{self, DATABASE, Started, Volume};

#[test]
#[ignore = "builds and runs the kestrel image"]
fn the_control_plane_is_what_the_image_starts_with_nothing_wrapped_around_it() {
    assert_eq!(
        control_plane::configured("{{json .Config.Entrypoint}}"),
        r#"["kestrel-control-plane"]"#
    );
    assert_eq!(control_plane::configured("{{json .Config.Cmd}}"), "null");
}

/// The Docker driver provisions an Environment by executing `docker` (ADR-0008), so the work
/// role in this image is only as real as the client beside it.
#[test]
#[ignore = "builds and runs the kestrel image"]
fn the_image_carries_the_client_its_compute_driver_executes() {
    let client = control_plane::running(&["docker", "--version"]);

    assert_eq!(client.code, 0, "docker in the image said {client:?}");
    assert!(
        client.out.starts_with("Docker version"),
        "docker in the image said {:?}",
        client.out
    );
}

/// The Client is installed where an operator is, never beside the database (ADR-0015).
#[test]
#[ignore = "builds and runs the kestrel image"]
fn the_image_carries_no_client() {
    for client in ["kestrel", "kestrel-client"] {
        let found = control_plane::running(&["sh", "-c", &format!("command -v {client}")]);

        assert_ne!(found.code, 0, "the image carries {client} at {}", found.out);
    }
}

#[test]
#[ignore = "builds and runs the kestrel image"]
fn no_argv_starts_every_role_in_one_process() {
    let volume = Volume::empty();

    let kestrel = Started::with(&volume, &[]);

    kestrel.wait_until_it_says("role=serve");
    kestrel.wait_until_it_says("role=work");
    kestrel.stop();
}

#[test]
#[ignore = "builds and runs the kestrel image"]
fn a_role_is_selected_by_argv_on_the_one_image() {
    let volume = Volume::empty();

    let serve = Started::with(&volume, &["serve"]);
    serve.wait_until_it_says("role=serve");
    serve.stop();
    assert!(
        !serve.said("role=work"),
        "`serve` started the work role too. it said:\n{}",
        serve.everything_it_said()
    );

    let work = Started::with(&volume, &["work"]);
    work.wait_until_it_says("role=work");
    work.stop();
    assert!(
        !work.said("role=serve"),
        "`work` started the serve role too. it said:\n{}",
        work.everything_it_said()
    );

    let refused = volume.run(&["organization", "list"]);
    assert_ne!(
        refused.code, 0,
        "the image ran an operator command in process: {refused:?}"
    );
}

/// Loopback is the binary's default and would leave the link reachable from nothing but the
/// container it is in, which is the one place nothing dials it from (ADR-0002).
#[test]
#[ignore = "builds and runs the kestrel image"]
fn the_link_the_image_serves_is_reachable_from_outside_the_container() {
    let volume = Volume::empty();
    let kestrel = Started::with(&volume, &["serve"]);
    kestrel.wait_until_it_says("role=serve");

    let answered = kestrel.what_the_link_answers();

    kestrel.stop();
    assert!(
        answered.starts_with("HTTP/"),
        "the link answered {answered:?} from outside the container"
    );
}

#[test]
#[ignore = "builds and runs the kestrel image"]
fn the_image_makes_and_migrates_its_database_on_a_volume_with_nothing_on_it() {
    let volume = Volume::empty();
    let kestrel = Started::with(&volume, &[]);

    let organization = ran(kestrel.operator(), &["organization", "declare", "acme"]);

    assert!(
        volume.holds(DATABASE),
        "the image kept its database somewhere the volume does not carry"
    );
    assert_eq!(
        ran(kestrel.operator(), &["organization", "list"]),
        format!("{organization}\tacme")
    );
    kestrel.stop();
}

/// What an upgrade replaces is the container, not the volume. No earlier image is published
/// to start one from, so this holds the database across the replacement rather than across
/// two versions of the migrations.
#[test]
#[ignore = "builds and runs the kestrel image"]
fn a_container_started_over_an_existing_database_migrates_it_and_loses_no_session() {
    let volume = Volume::empty();
    let first = Started::with(&volume, &["serve"]);
    let session = a_session(first.operator());
    let shown = session_shown(first.operator(), &session);
    let transcript = transcribed(first.operator(), &session);
    first.stop();

    let upgraded = Started::with(&volume, &["serve"]);

    assert_eq!(session_shown(upgraded.operator(), &session), shown);
    assert_eq!(transcribed(upgraded.operator(), &session), transcript);
    assert!(
        !transcript.is_empty(),
        "nothing was transcribed for the upgrade to keep"
    );
    upgraded.stop();
}

fn ran(operator: &str, command: &[&str]) -> String {
    let ran = client::ran(operator, command);
    assert!(
        ran.status.success(),
        "`kestrel {}` against the image failed:\n{}",
        command.join(" "),
        ran.err
    );

    ran.out.join("\n")
}

fn session_shown(operator: &str, session: &str) -> Vec<Value> {
    client::ran(
        operator,
        &[
            "session",
            "show",
            session,
            "--json",
            "id,name,state,checkout,opened_at",
        ],
    )
    .records()
}

fn transcribed(operator: &str, session: &str) -> Vec<Value> {
    client::ran(
        operator,
        &["session", "transcript", session, "--json", "seq,entry"],
    )
    .records()
}

fn a_session(operator: &str) -> String {
    ran(operator, &["organization", "declare", "acme"]);
    ran(
        operator,
        &[
            "workspace",
            "declare",
            "kestrel",
            "--repository",
            "https://github.com/jtmthf/kestrel",
            "--branch",
            "main",
        ],
    );
    ran(
        operator,
        &["agent", "declare", "builder", "--model", "claude-opus-5"],
    );

    ran(
        operator,
        &[
            "session",
            "open",
            "--workspace",
            "kestrel",
            "--agent",
            "builder",
        ],
    )
}
