//! The `kestrel` control-plane image: one artifact, every role selected by argv, over the
//! volume its database lives on.
//!
//! Every test here builds and runs the image, which a `cargo test` has no business doing on
//! its own, so they are ignored by default and CI runs them with `--ignored`.

mod support;

use support::control_plane::{self, DATABASE, Started, Volume};

#[test]
#[ignore = "builds and runs the kestrel image"]
fn the_control_plane_is_what_the_image_starts_with_nothing_wrapped_around_it() {
    assert_eq!(
        control_plane::configured("{{json .Config.Entrypoint}}"),
        r#"["kestrel"]"#
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

    let cli = volume.run(&["organization", "list"]);
    assert_eq!(cli.code, 0, "the CLI role in the image said {cli:?}");
}

/// Loopback is the binary's default and would leave the link reachable from nothing but the
/// container it is in, which is the one place nothing dials it from (ADR-0002).
#[test]
#[ignore = "builds and runs the kestrel image"]
fn the_link_the_image_serves_is_reachable_from_outside_the_container() {
    let volume = Volume::empty();
    let kestrel = Started::publishing_the_link(&volume, &["serve"]);
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

    let organization = volume.ran(&["organization", "declare", "acme"]);

    assert!(
        volume.holds(DATABASE),
        "the image kept its database somewhere the volume does not carry"
    );
    assert_eq!(
        volume.ran(&["organization", "list"]),
        format!("{organization}  acme")
    );
}

/// What an upgrade replaces is the container, not the volume. No earlier image is published
/// to start one from, so this holds the database across the replacement rather than across
/// two versions of the migrations.
#[test]
#[ignore = "builds and runs the kestrel image"]
fn a_container_started_over_an_existing_database_migrates_it_and_loses_no_session() {
    let volume = Volume::empty();
    let session = a_session(&volume);
    let shown = volume.ran(&["session", "show", &session]);
    let transcript = volume.ran(&["session", "transcript", &session]);

    let upgraded = Started::with(&volume, &[]);
    upgraded.wait_until_it_says("role=serve");
    upgraded.stop();

    assert_eq!(volume.ran(&["session", "show", &session]), shown);
    assert_eq!(volume.ran(&["session", "transcript", &session]), transcript);
    assert!(
        !transcript.is_empty(),
        "nothing was transcribed for the upgrade to keep"
    );
}

fn a_session(volume: &Volume) -> String {
    volume.ran(&["organization", "declare", "acme"]);
    volume.ran(&[
        "workspace",
        "declare",
        "kestrel",
        "--organization",
        "acme",
        "--repository",
        "https://github.com/jtmthf/kestrel",
        "--branch",
        "main",
    ]);
    volume.ran(&[
        "agent",
        "declare",
        "builder",
        "--organization",
        "acme",
        "--model",
        "claude-opus-5",
    ]);

    volume.ran(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ])
}
