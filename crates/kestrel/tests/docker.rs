//! The Docker `Compute` driver: the Run the primary test seam already drives, executed in a
//! real container provisioned from the `kestrel-env` image.
//!
//! Every test here builds and runs images, which a `cargo test` has no business doing on its
//! own, so they are ignored by default and CI runs them with `--ignored`.

mod support;

use std::time::Duration;

use kestrel::compute::{Docker, Driver};
use kestrel::domain::{Exit, Run, RunId, RunState, Session};
use support::Harness;
use support::image::{self, Container};
use support::scripted_agent::{self, Script};

const PATIENCE: Duration = Duration::from_secs(120);

/// A repository the container can reach, which a Workspace on this machine is not.
const REPOSITORY: &str = "https://github.com/jtmthf/kestrel";
const BRANCH: &str = "main";

async fn working(script: Script) -> Harness {
    Harness::dispatching_in(
        image::with_the_scripted_agent(),
        &scripted_agent::playing_in_an_image(script),
    )
    .await
}

async fn a_session(harness: &Harness) -> Session {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(&organization, "kestrel", &[REPOSITORY.to_owned()], BRANCH)
        .await;
    harness
        .declare_agent(
            &organization,
            "builder",
            "opencode",
            kestrel_scripted_agent::OTHER_MODEL,
        )
        .await;

    harness
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn ended(harness: &Harness, run: RunId) -> Run {
    until(harness, run, "ended", |run| run.state == RunState::Ended).await
}

async fn started(harness: &Harness, run: RunId) -> Run {
    until(harness, run, "started", |run| run.started_at.is_some()).await
}

/// The Run of ticket 06, dispatched at the driver the domain never names: the same script, the
/// same transcript, the same exit.
#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn the_scripted_run_ends_the_same_way_in_a_container_as_it_does_in_a_process() {
    let harness = working(Script::Speaks).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert_eq!(
        harness
            .transcript(session.id)
            .await
            .iter()
            .map(|entry| entry.entry.to_string())
            .filter(|entry| entry.starts_with("said"))
            .collect::<Vec<_>>(),
        vec![
            "said  builder  half of one message, and the other half".to_owned(),
            "said  builder  a second message".to_owned(),
        ]
    );

    harness.teardown().await;
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn an_environment_is_a_container_from_the_image_and_no_container_survives_the_run() {
    let harness = working(Script::Speaks).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    let environment = ended.environment.as_deref().expect("an environment");
    assert_eq!(
        environment,
        format!("docker/kestrel-{}", run.id),
        "a run names the container it executed in"
    );
    Container::named(environment).is_gone().await;

    harness.teardown().await;
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn a_workspaces_repositories_and_its_branch_are_in_the_container() {
    let harness = working(Script::Dawdles).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let container = Container::named(
        started(&harness, run.id)
            .await
            .environment
            .as_deref()
            .expect("an environment"),
    );

    let branch = container.exec(&[
        "git",
        "-C",
        "/workspace/kestrel",
        "branch",
        "--show-current",
    ]);
    assert_eq!(
        branch.out, BRANCH,
        "the workspace is not on its branch: {branch:?}"
    );
    let readme = container.exec(&["test", "-f", "/workspace/kestrel/README.md"]);
    assert_eq!(
        readme.code, 0,
        "the repository is not in the workspace: {readme:?}"
    );

    harness.teardown().await;
    container.is_gone().await;
}

/// A container that dies takes the supervisor holding the Run's lease out with it, so the Run
/// cannot go on; the work role attending it sees the container gone before the lease it stopped
/// holding out is due, and that is what ends it.
#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn a_container_that_dies_mid_run_is_detected_and_no_container_survives_the_failure() {
    let harness = working(Script::Dawdles).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let container = Container::named(
        started(&harness, run.id)
            .await
            .environment
            .as_deref()
            .expect("an environment"),
    );

    container.kill();

    let ended = ended(&harness, run.id).await;
    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and its container was killed",
            ended.exit
        );
    };
    assert!(
        because.contains("without reporting how the run went"),
        "unhelpful exit status: {because}"
    );
    assert!(
        ended.lease_expires_at.is_none(),
        "a run whose container died still holds a lease"
    );
    container.is_gone().await;

    harness.teardown().await;
}

/// The six operations against a real container, including the two nothing at 0.1 calls: a
/// driver that implemented only what the work role happens to reach for would be a driver that
/// has to grow to meet the contract later.
#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn every_operation_in_the_contract_works_against_a_container() {
    let harness = Harness::boot_reachable_from_an_environment().await;
    let session = a_session(&harness).await;
    let (run, credential) = harness.dispatch_run(session.id).await;

    // Provisioned through the port rather than through the work role, so the operations no
    // Run makes are exercised on the same Environment as the ones it does.
    let mut environment = Driver::Docker(Docker::provisioning_from(image::built()))
        .provision(
            run.id,
            &[
                ("KESTREL_LINK", &harness.link_from_an_environment()),
                ("KESTREL_RUN", &run.id.to_string()),
                ("KESTREL_RUN_CREDENTIAL", credential.as_str()),
                ("KESTREL_AGENT_RUNTIME", "opencode acp"),
            ],
        )
        .expect("the environment should provision");
    let container = Container::named(environment.name());

    assert_eq!(
        environment.status().expect("the status should read"),
        None,
        "a container that was just started has already ended"
    );

    environment
        .write_file("wrote/file", b"from outside the container")
        .expect("the file should write");
    assert_eq!(
        environment.read_file("wrote/file").expect("a read"),
        b"from outside the container"
    );

    let listed = environment
        .exec(&["ls", "wrote"])
        .expect("ls should exec")
        .finish()
        .expect("ls should finish");
    assert!(listed.exited.success(), "ls said {listed:?}");
    assert_eq!(listed.out, "file");

    environment.destroy().expect("the container should destroy");
    container.is_gone().await;

    harness.teardown().await;
}
