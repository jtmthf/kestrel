//! The `kestrel-env` image: the base image a Run executes in, and an Environment that is a
//! container dialling out to a control plane on this machine.
//!
//! Every test here builds and runs the image, which a `cargo test` has no business doing on
//! its own, so they are ignored by default and CI runs them with `--ignored`.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Exit, Run, RunId, RunState, Session};
use kestrel::link::credential::Secret;
use support::Harness;
use support::image::{self, Container};

const PATIENCE: Duration = Duration::from_secs(30);

const KILLED: i32 = 137;

#[test]
#[ignore = "builds and runs the kestrel-env image"]
fn the_supervisor_the_agent_runtime_and_git_are_each_invocable_in_the_image() {
    let git = image::running(&["git", "--version"]);
    assert!(
        git.out.starts_with("git version"),
        "git in the image said {:?}",
        git.out
    );

    let opencode = image::running(&["opencode", "--version"]);
    assert_eq!(opencode.code, 0, "opencode in the image said {opencode:?}");
    assert!(
        !opencode.out.is_empty(),
        "opencode in the image named no version"
    );

    let supervisor = image::running(&["kestrel-supervisor"]);
    assert!(
        supervisor.err.contains("no link to dial"),
        "the supervisor in the image said {:?}",
        supervisor.err
    );
}

#[test]
#[ignore = "builds and runs the kestrel-env image"]
fn the_image_carries_no_node_runtime_and_no_claude_binary() {
    let found = image::running(&[
        "find", "/", "-xdev", "!", "-type", "d", "(", "-name", "node", "-o", "-name", "nodejs",
        "-o", "-name", "npm", "-o", "-name", "npx", "-o", "-name", "bun", "-o", "-name", "claude",
        ")",
    ]);

    assert!(
        found.out.is_empty(),
        "the image carries what ADR-0007 took out of it:\n{}",
        found.out
    );
}

#[test]
#[ignore = "builds and runs the kestrel-env image"]
fn the_image_exposes_no_inbound_port() {
    assert_eq!(image::configured("{{json .Config.ExposedPorts}}"), "null");
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn an_environment_that_is_a_container_dials_out_and_the_control_plane_knows_it_is_connected()
{
    let harness = Harness::boot_reachable_from_a_container().await;
    let (run, credential) = a_run(&harness).await;

    let mut container = a_container(&harness, run.id, &credential);
    container.wait_until_it_says("reported connected").await;

    let connected = harness
        .run(run.id)
        .await
        .connected
        .expect("the control plane should know a container is on the link");
    assert!(
        !connected.version.is_empty(),
        "the control plane learned that something connected, but not what"
    );

    container.destroy();
    harness.teardown().await;
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn killing_the_supervisor_in_the_container_ends_the_run_and_nothing_restarts_it() {
    let harness = Harness::boot_reachable_from_a_container().await;
    let (run, credential) = a_run(&harness).await;

    let mut container = a_container(&harness, run.id, &credential);
    container.wait_until_it_says("reported connected").await;

    container.kill_the_supervisor();

    assert_eq!(
        container.exits().await,
        KILLED,
        "the container outlived the supervisor. it said:\n{}",
        container.everything_it_said()
    );
    assert_eq!(container.state(), "exited");
    assert_eq!(
        container.restarts(),
        0,
        "the supervisor was restarted under its Run"
    );

    harness.lease_until(&run, a_moment_ago()).await;
    let ended = until(&harness, run.id, "ended", |run| {
        run.state == RunState::Ended
    })
    .await;
    let Some(Exit::Failed { because }) = ended.exit else {
        panic!(
            "the run ended {:?}, and the supervisor holding its lease out was killed",
            ended.exit
        );
    };
    assert!(
        because.contains("lease"),
        "a run whose supervisor was killed fails by its lease: {because}"
    );

    container.destroy();
    harness.teardown().await;
}

fn a_container(harness: &Harness, run: RunId, credential: &Secret) -> Container {
    Container::provision(&harness.link_from_a_container(), run, credential)
}

async fn a_run(harness: &Harness) -> (Run, Secret) {
    let session = a_session(harness).await;

    harness.dispatch_run(session.id).await
}

async fn a_session(harness: &Harness) -> Session {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &organization,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    harness
        .declare_agent(&organization, "builder", "opencode", "claude-opus-5")
        .await;

    harness.open_session("acme", "kestrel", "builder").await
}

async fn until(harness: &Harness, run: RunId, what: &str, ready: impl Fn(&Run) -> bool) -> Run {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let run = harness.run(run).await;
        if ready(&run) {
            return run;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run {} is {} with the exit status {:?}, and never {what}",
            run.id,
            run.state,
            run.exit
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn a_moment_ago() -> Timestamp {
    Timestamp::now() - SignedDuration::from_secs(1)
}
