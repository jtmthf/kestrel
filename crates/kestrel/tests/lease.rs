//! The run-held lease and the sweep that reaps it: a Run holds one from the moment it is
//! claimed, its Environment holds it out for as long as it is alive, and a lease nothing
//! holds out ends its Run failed rather than leaving a Session wedged.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Exit, Run, RunId, RunState, Session};
use support::environment::Environment;
use support::scripted_agent::Script;
use support::{Harness, scripted_agent, supervisor};

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

async fn swept(harness: &Harness, run: RunId) -> String {
    let ended = until(harness, run, "ended", |run| run.state == RunState::Ended).await;

    let Some(Exit::Failed { because }) = ended.exit else {
        panic!(
            "the run ended {:?}, and nothing was holding its lease out",
            ended.exit
        );
    };
    assert!(
        because.contains("lease"),
        "a run failed by its lease says so: {because}"
    );
    assert!(
        ended.lease_expires_at.is_none(),
        "a run that has ended still holds a lease"
    );

    because
}

fn a_moment_ago() -> Timestamp {
    Timestamp::now() - SignedDuration::from_secs(1)
}

/// A lease due sooner than a real one, and further off than an Environment that is alive lets
/// one get.
fn shortened() -> Timestamp {
    Timestamp::now() + SignedDuration::from_secs(4)
}

#[tokio::test]
async fn a_run_holds_a_lease_from_the_moment_it_is_claimed() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;

    let queued = harness.enqueue_run(session.id).await;
    assert!(queued.lease_expires_at.is_none());

    let claimed = harness.claim_run().await.expect("a run to claim").run;
    assert_eq!(claimed.id, queued.id);
    assert!(
        claimed.lease_expires_at > Some(Timestamp::now()),
        "a claimed run holds no lease"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_lease_nothing_holds_out_ends_its_run_failed() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;

    harness.lease_until(&run, a_moment_ago()).await;

    let because = swept(&harness, run.id).await;
    assert_eq!(
        harness
            .transcript(session.id)
            .await
            .last()
            .expect("a transcript entry")
            .entry
            .to_string(),
        format!("run ended  {}  failed: {because}", run.id)
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_lease_that_expires_leaves_its_session_no_active_run() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;

    harness.lease_until(&run, a_moment_ago()).await;
    swept(&harness, run.id).await;

    assert!(
        harness
            .runs(session.id)
            .await
            .iter()
            .all(|run| run.state != RunState::Active),
        "a session whose run's lease expired still has an active run"
    );
    let next = harness.enqueue_run(session.id).await;
    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(next.id),
        "the run after the one that expired was not dispatched"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_failed_by_lease_expiry_is_never_dispatched_again() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;

    harness.lease_until(&run, a_moment_ago()).await;
    swept(&harness, run.id).await;

    assert!(
        harness.claim_run().await.is_none(),
        "a run failed by its lease expiring was handed out to be dispatched again"
    );
    assert_eq!(harness.run(run.id).await.state, RunState::Ended);

    harness.teardown().await;
}

#[tokio::test]
async fn a_due_time_survives_a_control_plane_restart_and_fires_after_it() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;

    let stopped = harness.kill().await;
    stopped.lease_until(&run, a_moment_ago()).await;
    let harness = stopped.restart().await;

    swept(&harness, run.id).await;

    harness.teardown().await;
}

#[tokio::test]
async fn an_environment_holds_its_runs_lease_out_for_the_life_of_the_run() {
    let harness = Harness::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Dawdles),
    )
    .await;
    let session = a_session(&harness).await;
    let run = harness.enqueue_run(session.id).await;

    let working = until(&harness, run.id, "started", |run| run.started_at.is_some()).await;
    let shortened = shortened();
    harness.lease_until(&working, shortened).await;

    let held = until(&harness, run.id, "had its lease held out", |run| {
        run.lease_expires_at > Some(shortened)
    })
    .await;
    assert_eq!(held.state, RunState::Active);

    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        harness.run(run.id).await.state,
        RunState::Active,
        "a run whose environment is alive was swept anyway"
    );

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn an_environment_that_dies_mid_run_stops_holding_the_lease_out_and_the_run_ends_failed() {
    // The Environment outlives the supervisor inside it, so what ends this Run is the lease
    // rather than the work role noticing an Environment that is gone.
    let environment = Environment::executing(&format!(
        "\"{}\" &\nsupervisor=$!\nsleep 3\nkill -9 $supervisor\nsleep 60",
        supervisor::binary().display()
    ));
    let harness = Harness::dispatching_to(
        environment.path(),
        &scripted_agent::playing(Script::Dawdles),
    )
    .await;
    let session = a_session(&harness).await;
    let run = harness.enqueue_run(session.id).await;

    let working = until(&harness, run.id, "started", |run| run.started_at.is_some()).await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    // The same lease the Environment above outlives: an Environment still alive holds one out
    // well inside this, so what ends this Run is the supervisor inside it being gone.
    harness.lease_until(&working, shortened()).await;

    swept(&harness, run.id).await;
    Environment::named(working.environment.as_deref().expect("an environment"))
        .is_gone()
        .await;

    harness.teardown().await;
}
