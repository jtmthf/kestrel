//! The `kestrel-env` image: the base image a Run executes in, and an Environment provisioned
//! from it, dialling out to a control plane on this machine.
//!
//! Every test here builds and runs the image, which a `cargo test` has no business doing on
//! its own, so they are ignored by default and CI runs them with `--ignored`.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Exit, Run, RunId, RunState, Session};
use kestrel::link::credential::Secret;
use support::Harness;
use support::image::{self, Environment};

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
    assert!(
        !anything_named(&["kestrel-supervisor"]).is_empty(),
        "the sweep found nothing at all, so it proves nothing about what is absent"
    );

    let found = anything_named(&["node", "nodejs", "npm", "npx", "bun", "claude"]);

    assert!(
        found.is_empty(),
        "the image carries what ADR-0007 took out of it:\n{found}"
    );
}

#[test]
#[ignore = "builds and runs the kestrel-env image"]
fn the_image_exposes_no_inbound_port() {
    assert_eq!(image::configured("{{json .Config.ExposedPorts}}"), "null");
}

#[test]
#[ignore = "builds and runs the kestrel-env image"]
fn the_supervisor_is_what_the_image_starts_with_nothing_wrapped_around_it() {
    assert_eq!(
        image::configured("{{json .Config.Entrypoint}}"),
        r#"["kestrel-supervisor"]"#
    );
    assert_eq!(image::configured("{{json .Config.Cmd}}"), "null");
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn an_environment_the_image_provisions_dials_out_and_the_control_plane_knows_it_is_connected()
{
    let harness = Harness::boot_reachable_from_an_environment().await;
    let (run, credential) = a_run(&harness).await;

    let mut environment = an_environment(&harness, run.id, &credential);
    environment.wait_until_it_says("reported connected").await;

    let connected = harness
        .run(run.id)
        .await
        .connected
        .expect("the control plane should know an environment is on the link");
    assert!(
        !connected.version.is_empty(),
        "the control plane learned that something connected, but not what"
    );

    environment.destroy();
    harness.teardown().await;
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn killing_the_supervisor_in_the_environment_ends_the_run_and_nothing_restarts_it() {
    let harness = Harness::boot_reachable_from_an_environment().await;
    let (run, credential) = a_run(&harness).await;

    let mut environment = an_environment(&harness, run.id, &credential);
    environment.wait_until_it_says("reported connected").await;

    environment.kill_the_supervisor();

    assert_eq!(
        environment.exits().await,
        KILLED,
        "the environment outlived the supervisor. it said:\n{}",
        environment.everything_it_said()
    );
    assert_eq!(
        environment.state(),
        "exited",
        "something brought the supervisor back under its Run"
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

    environment.destroy();
    harness.teardown().await;
}

fn anything_named(names: &[&str]) -> String {
    let mut sweep = vec!["find", "/", "-xdev", "!", "-type", "d", "("];
    for (nth, name) in names.iter().enumerate() {
        if nth > 0 {
            sweep.push("-o");
        }
        sweep.extend_from_slice(&["-name", name]);
    }
    sweep.push(")");

    image::running(&sweep).out
}

fn an_environment(harness: &Harness, run: RunId, credential: &Secret) -> Environment {
    Environment::provision(&harness.link_from_an_environment(), run, credential)
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
