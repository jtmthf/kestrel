//! The Docker `Compute` driver: the Run the primary test seam already drives, executed in a
//! real container provisioned from the `kestrel-env` image.
//!
//! Every test here builds and runs images, which a `cargo test` has no business doing on its
//! own, so they are ignored by default and CI runs them with `--ignored`.

mod support;

use std::time::Duration;

use kestrel::compute::{Docker, Driver};
use kestrel::domain::{Exit, Run, RunId, Session, SessionId};
use support::Kestrel;
use support::image::{self, Container};
use support::scripted_agent::{self, Script};

const PATIENCE: Duration = Duration::from_secs(120);

/// A repository the container can reach, which one on this machine is not.
const REPOSITORY: &str = "https://github.com/jtmthf/kestrel";
const BRANCH: &str = "main";

async fn working(script: Script) -> Kestrel {
    Kestrel::dispatching_in(
        image::with_the_scripted_agent(),
        &scripted_agent::playing_in_an_image(script),
    )
    .await
}

async fn a_session(kestrel: &Kestrel) -> Session {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(&organization, "kestrel", &[REPOSITORY.to_owned()], BRANCH)
        .await;
    kestrel
        .declare_agent(
            &organization,
            "builder",
            "opencode",
            Some(kestrel_scripted_agent::OTHER_MODEL),
        )
        .await;

    kestrel
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;

    kestrel.open_session("acme", "kestrel", "builder").await
}

async fn until(kestrel: &Kestrel, run: RunId, what: &str, ready: impl Fn(&Run) -> bool) -> Run {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let run = kestrel.run(run).await;
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

/// Answering a turn never ends a Run, so one that answered is stopped, the way a person would.
async fn ended(kestrel: &Kestrel, run: RunId) -> Run {
    kestrel.after_one_turn_within(run, PATIENCE).await
}

/// A stopped Run's exit is recorded before its supervisor has actually left (ADR-0024), so its
/// container is given a moment to catch up before this looks for what it left behind.
async fn without_its_processes(container: &Container) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    loop {
        let left = container.processes();
        let clean =
            !left.contains("kestrel-supervisor") && !left.contains("kestrel-scripted-agent");
        if clean || tokio::time::Instant::now() >= deadline {
            return left;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn started(kestrel: &Kestrel, run: RunId) -> Run {
    until(kestrel, run, "started", |run| run.started_at.is_some()).await
}

/// A Run's exit is recorded as soon as it is decided, before its supervisor is confirmed gone;
/// a session does not free its slot until that confirmation lands, which for a dead container
/// can take a reconciliation pass rather than the commit that ended the Run (ADR-0002).
async fn enqueue_when_free(kestrel: &Kestrel, session: SessionId) -> Run {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        match kestrel.try_enqueue_run(session).await {
            Ok(run) => return run,
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the run should enqueue: {error}"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// The Run of ticket 06, dispatched at the driver the domain never names: the same script, the
/// same transcript, the same exit.
#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn the_scripted_run_ends_the_same_way_in_a_container_as_it_does_in_a_process() {
    let kestrel = working(Script::Speaks).await;
    let session = a_session(&kestrel).await;

    let run = kestrel.enqueue_run(session.id).await;
    let ended = ended(&kestrel, run.id).await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert_eq!(
        kestrel
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

    kestrel.teardown().await;
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn an_instance_is_a_container_that_outlives_its_run_but_not_its_supervisor() {
    let kestrel = working(Script::Speaks).await;
    let session = a_session(&kestrel).await;

    let run = kestrel.enqueue_run(session.id).await;
    let ended = ended(&kestrel, run.id).await;

    let instance = ended.instance.as_deref().expect("an instance");
    assert_eq!(
        instance,
        format!("docker/kestrel-{}", run.id),
        "a run names the container it executed on"
    );
    let container = Container::named(instance);
    let left = without_its_processes(&container).await;
    assert!(
        !left.contains("kestrel-supervisor") && !left.contains("kestrel-scripted-agent"),
        "the run left processes on its instance: {left}"
    );

    kestrel.teardown().await;
    container.is_gone().await;
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn a_projects_repositories_and_its_branch_are_in_the_container() {
    let kestrel = working(Script::Dawdles).await;
    let session = a_session(&kestrel).await;

    let run = kestrel.enqueue_run(session.id).await;
    let container = Container::named(
        started(&kestrel, run.id)
            .await
            .instance
            .as_deref()
            .expect("an instance"),
    );

    let branch = container.exec(&[
        "git",
        "-C",
        "/workspace/kestrel",
        "branch",
        "--show-current",
    ]);
    assert_eq!(
        branch.out, session.checkout.branch,
        "the workspace is not on its session's branch: {branch:?}"
    );
    let readme = container.exec(&["test", "-f", "/workspace/kestrel/README.md"]);
    assert_eq!(
        readme.code, 0,
        "the repository is not in the workspace: {readme:?}"
    );

    kestrel.teardown().await;
    container.is_gone().await;
}

/// A container that dies takes the supervisor holding the Run's lease out with it, so the Run
/// cannot go on; the work role attending it sees the supervisor gone before the lease it stopped
/// holding out is due, and that is what ends it.
#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn a_container_that_dies_mid_run_is_detected_and_the_next_run_starts_it_again() {
    let kestrel = working(Script::Dawdles).await;
    let session = a_session(&kestrel).await;

    let run = kestrel.enqueue_run(session.id).await;
    let container = Container::named(
        started(&kestrel, run.id)
            .await
            .instance
            .as_deref()
            .expect("an instance"),
    );

    container.kill();

    let ended = ended(&kestrel, run.id).await;
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

    let next = enqueue_when_free(&kestrel, session.id).await;
    let next = started(&kestrel, next.id).await;
    assert_eq!(
        next.instance, ended.instance,
        "a stopped container was taken for gone"
    );

    kestrel.teardown().await;
    container.is_gone().await;
}

/// Every operation against a real container, including the ones no Run makes: a
/// driver that implemented only what the work role happens to reach for would be a driver that
/// has to grow to meet the contract later.
#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn every_operation_in_the_contract_works_against_a_container() {
    let kestrel = Kestrel::boot_reachable_from_an_environment().await;
    let session = a_session(&kestrel).await;
    let (run, credential) = kestrel.dispatch_run(session.id).await;

    // Provisioned through the port rather than through the work role, so the operations no
    // Run makes are exercised on the same Instance as the ones it does.
    let driver = Driver::Docker(Docker::provisioning_from(image::built()));
    let mut instance = driver
        .provision(run.id)
        .expect("the instance should provision");
    let container = Container::named(instance.name());
    let mut supervisor = instance
        .supervise(&[
            ("KESTREL_LINK", &kestrel.link_from_an_environment()),
            ("KESTREL_RUN", &run.id.to_string()),
            ("KESTREL_RUN_CREDENTIAL", credential.as_str()),
            ("KESTREL_AGENT_RUNTIME", "opencode acp"),
        ])
        .expect("the supervisor should start");

    assert_eq!(
        supervisor.status().expect("the status should read"),
        None,
        "a supervisor that was just started has already ended"
    );
    driver
        .stop_named(supervisor.name())
        .expect("the supervisor should stop");
    supervisor.stop().expect("the supervisor should stop");
    assert!(
        container
            .exec(&["sh", "-c", "env | grep KESTREL_RUN_CREDENTIAL"])
            .code
            != 0,
        "the run's credential outlived its supervisor"
    );

    let mut instance = driver
        .resume(instance.name())
        .expect("the instance should resume")
        .expect("the instance should still be there");
    instance
        .write_file("wrote/file", b"from outside the container")
        .expect("the file should write");
    assert_eq!(
        instance.read_file("wrote/file").expect("a read"),
        b"from outside the container"
    );

    let listed = instance
        .exec(&["ls", "wrote"])
        .expect("ls should exec")
        .finish()
        .expect("ls should finish");
    assert!(listed.exited.success(), "ls said {listed:?}");
    assert_eq!(listed.out, "file");

    let name = instance.name().to_owned();
    instance.destroy().expect("the container should destroy");
    container.is_gone().await;
    assert!(
        driver
            .resume(&name)
            .expect("a gone container is not an error")
            .is_none()
    );

    kestrel.teardown().await;
}
