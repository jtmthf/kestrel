//! A Run from enqueued to ended, driven through the primary test seam: the work role claims
//! it, a local-exec Environment executes it, and it ends with an exit status.

mod support;

use std::path::Path;
use std::time::Duration;

use kestrel::domain::{Exit, Run, RunId, RunState, Session};
use support::Harness;
use support::environment::Environment;
use support::supervisor;

const PATIENCE: Duration = Duration::from_secs(30);

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

async fn ended(harness: &Harness, run: RunId) -> Run {
    until(harness, run, "ended", |run| run.state == RunState::Ended).await
}

#[tokio::test]
async fn a_run_enqueued_is_claimed_dispatched_and_reaches_an_environment() {
    let harness = Harness::dispatching(supervisor::binary()).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    assert_eq!(run.state, RunState::Queued);
    let ended = ended(&harness, run.id).await;

    assert!(
        ended.connected.is_some(),
        "the run ended without an environment ever reaching the link"
    );
    assert!(ended.environment.is_some());
    assert_eq!(ended.exit, Some(Exit::Succeeded));

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_that_reaches_an_environment_starts_and_ends_in_the_transcript() {
    let harness = Harness::dispatching(supervisor::binary()).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    ended(&harness, run.id).await;

    let said: Vec<String> = harness
        .transcript(session.id)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect();

    assert_eq!(
        said,
        vec![
            "participant joined  builder".to_owned(),
            format!("run started  {}", run.id),
            format!("run ended  {}  succeeded", run.id),
        ]
    );

    harness.teardown().await;
}

#[tokio::test]
async fn the_environment_a_finished_run_executed_in_is_destroyed() {
    let harness = Harness::dispatching(supervisor::binary()).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    Environment::named(ended.environment.as_deref().expect("an environment"))
        .is_gone()
        .await;

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn an_environment_that_ends_without_saying_how_the_run_went_leaves_it_failed() {
    let environment = Environment::executing("exit 3");
    let harness = Harness::dispatching(environment.path()).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and its environment reported nothing",
            ended.exit
        );
    };
    assert!(
        because.contains("without reporting how the run went"),
        "unhelpful exit status: {because}"
    );

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_still_in_flight_when_the_control_plane_stops_ends_and_its_environment_is_destroyed()
{
    let environment = Environment::executing("sleep 300");
    let harness = Harness::dispatching(environment.path()).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let in_flight = until(&harness, run.id, "reached an environment", |run| {
        run.environment.is_some() && run.heartbeat_at.is_some()
    })
    .await;

    let stopped = harness.teardown().await;

    let ended = stopped.run(run.id).await;
    assert_eq!(ended.state, RunState::Ended);
    assert!(matches!(ended.exit, Some(Exit::Failed { .. })));
    Environment::named(in_flight.environment.as_deref().expect("an environment"))
        .is_gone()
        .await;
}

#[tokio::test]
async fn a_run_whose_environment_cannot_be_provisioned_ends_rather_than_staying_queued() {
    let harness = Harness::dispatching(Path::new("/nowhere/kestrel-supervisor")).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!("the run ended {:?}, and nothing provisioned it", ended.exit);
    };
    assert!(
        because.contains("could not be provisioned"),
        "unhelpful exit status: {because}"
    );
    assert!(ended.environment.is_none());

    harness.teardown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_run_is_claimed_once_however_many_claimants_ask_at_once() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let run = harness.enqueue_run(session.id).await;

    let (first, second) = tokio::join!(harness.claim_run(), harness.claim_run());

    let claimed: Vec<RunId> = [first, second]
        .into_iter()
        .flatten()
        .map(|claimed| claimed.run.id)
        .collect();
    assert_eq!(claimed, vec![run.id]);

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_that_ended_is_never_claimed_again() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;

    harness.complete_run(&run).await;

    assert!(
        harness.claim_run().await.is_none(),
        "a run that already ended was handed out to be dispatched again"
    );

    harness.teardown().await;
}
