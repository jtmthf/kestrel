//! The run-held lease and the sweep that reaps it: a Run holds one from the moment it is
//! claimed, its Environment holds it out for as long as it is alive, and a lease nothing
//! holds out ends its Run failed rather than leaving a Workspace wedged.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Exit, Run, RunId, RunState, Workspace};
use support::environment::Environment;
use support::repository;
use support::scripted_agent::Script;
use support::{Kestrel, scripted_agent, supervisor};

const PATIENCE: Duration = Duration::from_secs(30);

async fn a_workspace(kestrel: &Kestrel) -> Workspace {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(
            &organization,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
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

    kestrel.open_workspace("acme", "kestrel", "builder").await
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
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn swept(kestrel: &Kestrel, run: RunId) -> String {
    let ended = until(kestrel, run, "ended", |run| run.state == RunState::Ended).await;

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
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;

    let queued = kestrel.enqueue_run(workspace.id).await;
    assert!(queued.lease_expires_at.is_none());

    let claimed = kestrel.claim_run().await.expect("a run to claim").run;
    assert_eq!(claimed.id, queued.id);
    assert!(
        claimed.lease_expires_at > Some(Timestamp::now()),
        "a claimed run holds no lease"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_lease_nothing_holds_out_ends_its_run_failed() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;

    kestrel.lease_until(&run, a_moment_ago()).await;

    let because = swept(&kestrel, run.id).await;
    assert_eq!(
        kestrel
            .transcript(workspace.id)
            .await
            .last()
            .expect("a transcript entry")
            .entry
            .to_string(),
        format!("run ended  {}  failed: {because}", run.id)
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_lease_that_expires_leaves_its_workspace_no_active_run() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;

    kestrel.lease_until(&run, a_moment_ago()).await;
    swept(&kestrel, run.id).await;

    assert!(
        kestrel
            .runs(workspace.id)
            .await
            .iter()
            .all(|run| run.state != RunState::Working),
        "a workspace whose run's lease expired still has an active run"
    );
    let next = kestrel.enqueue_run(workspace.id).await;
    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(next.id),
        "the run after the one that expired was not dispatched"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn one_parallel_runs_expired_lease_leaves_the_other_run_active() {
    let kestrel = Kestrel::dispatching_up_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Dawdles),
        2,
    )
    .await;
    let first_workspace = a_workspace(&kestrel).await;
    let second_workspace = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let first = kestrel.enqueue_run(first_workspace.id).await;
    let second = kestrel.enqueue_run(second_workspace.id).await;
    let first = until(&kestrel, first.id, "started", |run| {
        run.started_at.is_some()
    })
    .await;
    until(&kestrel, second.id, "started", |run| {
        run.started_at.is_some()
    })
    .await;

    kestrel.lease_until(&first, a_moment_ago()).await;
    swept(&kestrel, first.id).await;

    assert_eq!(kestrel.run(second.id).await.state, RunState::Working);

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_failed_by_lease_expiry_is_never_dispatched_again() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;

    kestrel.lease_until(&run, a_moment_ago()).await;
    swept(&kestrel, run.id).await;

    assert!(
        kestrel.claim_run().await.is_none(),
        "a run failed by its lease expiring was handed out to be dispatched again"
    );
    assert_eq!(kestrel.run(run.id).await.state, RunState::Ended);

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_due_time_survives_a_control_plane_restart_and_fires_after_it() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;

    let stopped = kestrel.kill().await;
    stopped.lease_until(&run, a_moment_ago()).await;
    let kestrel = stopped.restart().await;

    swept(&kestrel, run.id).await;

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_supervisor_holds_its_runs_lease_out_for_the_life_of_the_run() {
    let kestrel = Kestrel::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Dawdles),
    )
    .await;
    let workspace = a_workspace(&kestrel).await;
    let run = kestrel.enqueue_run(workspace.id).await;

    let working = until(&kestrel, run.id, "started", |run| run.started_at.is_some()).await;
    let shortened = shortened();
    kestrel.lease_until(&working, shortened).await;

    let held = until(&kestrel, run.id, "had its lease held out", |run| {
        run.lease_expires_at > Some(shortened)
    })
    .await;
    assert_eq!(held.state, RunState::Working);

    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        kestrel.run(run.id).await.state,
        RunState::Working,
        "a run whose supervisor is alive was swept anyway"
    );

    kestrel.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_supervisor_that_dies_mid_run_stops_holding_the_lease_out_and_the_run_ends_failed() {
    // The script outlives the supervisor it started, so what ends this Run is the lease rather
    // than the work role noticing a supervisor that is gone.
    let environment = Environment::executing(&format!(
        "\"{}\" &\nsupervisor=$!\nsleep 3\nkill -9 $supervisor\nsleep 60",
        supervisor::binary().display()
    ));
    let kestrel = Kestrel::dispatching_to(
        environment.path(),
        &scripted_agent::playing(Script::Dawdles),
    )
    .await;
    let workspace = a_workspace(&kestrel).await;
    let run = kestrel.enqueue_run(workspace.id).await;

    let working = until(&kestrel, run.id, "started", |run| run.started_at.is_some()).await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    // The same lease the script above outlives: a supervisor still alive holds one out well
    // inside this, so what ends this Run is the supervisor being gone.
    kestrel.lease_until(&working, shortened()).await;

    swept(&kestrel, run.id).await;
    Environment::named(working.supervisor.as_deref().expect("a supervisor"))
        .is_gone()
        .await;

    kestrel.teardown().await;
}
