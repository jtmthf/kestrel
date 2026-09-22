//! An Instance is archived when its Session seals only if a real checkout says everything in it
//! can be recovered from the remote; one that may hold the only copy of some work is held until a
//! person releases it.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Exit, Run, RunState, Session, SessionState};
use support::Harness;
use support::environment::Environment;
use support::repository;
use support::scripted_agent::{self, Script};
use support::supervisor;

const PATIENCE: Duration = Duration::from_secs(30);

const COMMIT: &str = "git -C kestrel -c user.name=kestrel -c user.email=kestrel@example.com \
                      commit --quiet";

async fn a_session(harness: &Harness) -> Session {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &organization,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;
    harness
        .declare_agent(
            &organization,
            "builder",
            "opencode",
            Some(kestrel_scripted_agent::OTHER_MODEL),
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

/// Stands in for the Agent Runtime: does something to the checkout, then hands over to the agent.
#[cfg(unix)]
fn working(shell: &str) -> Environment {
    Environment::executing(&format!(
        "{{ {shell}; }} >&2\nexec {}",
        scripted_agent::playing(Script::Speaks)
    ))
}

#[cfg(unix)]
async fn dispatching_to(runtime: &Environment) -> Harness {
    Harness::dispatching_to(
        supervisor::binary(),
        &format!("\"{}\"", runtime.path().display()),
    )
    .await
}

/// Ended, and with its supervisor gone, so nothing but what its checkout holds keeps its Session.
/// A Run that answers rather than failing waits between turns until something stops it (ADR-0024),
/// so this stops it itself once it has answered, the way a person or a seal would.
async fn over(harness: &Harness, session: &Session) -> Run {
    let run = harness.enqueue_run(session.id).await;
    let answered = harness.answered(run.id, 1).await;
    if answered.state != RunState::Ended {
        harness.stop_run(run.id).await;
    }

    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let ended = harness.run(run.id).await;
        if ended.state == RunState::Ended && harness.supervisors_to_stop().await.is_empty() {
            return ended;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run {} is {} and never finished",
            run.id,
            ended.state
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn a_day_ago() -> Timestamp {
    Timestamp::now() - SignedDuration::from_hours(25)
}

async fn eventually(what: &str, done: impl AsyncFn() -> bool) {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    while !done().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} never happened"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn archived(instance: &str) {
    let workspace = Environment::workspace_of(instance);
    eventually(&format!("archiving {instance}"), async || {
        !workspace.exists()
    })
    .await;
}

/// Long enough that a sweep that was going to seal this Session has run several times over.
async fn stays_open(harness: &Harness, session: &Session) {
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        harness.show_session(session.id).await.state,
        SessionState::Open,
        "the session {} sealed itself with its instance holding work",
        session.id
    );
}

#[cfg(unix)]
#[tokio::test]
async fn clean_research_work_seals_when_idle_and_its_instance_is_archived() {
    let runtime = working(
        "echo target/ >> kestrel/.git/info/exclude; mkdir -p kestrel/target; \
         echo built > kestrel/target/output",
    );
    let harness = dispatching_to(&runtime).await;
    let session = a_session(&harness).await;
    let run = over(&harness, &session).await;
    let instance = run.instance.expect("an instance");
    assert!(harness.held_instances("acme").await.is_empty());

    harness.last_active(&session, a_day_ago()).await;

    let session_id = session.id;
    eventually("the idle sweep sealing the session", async || {
        harness.show_session(session_id).await.state == SessionState::Sealed
    })
    .await;
    archived(&instance).await;
    assert_eq!(harness.instance(session.id).await, None);

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_pushed_checkout_is_archived_when_its_session_seals() {
    let runtime = working(&format!(
        "echo committed > kestrel/committed; git -C kestrel add committed; \
         {COMMIT} --message 'pushed work'; git -C kestrel push --quiet origin HEAD"
    ));
    let harness = dispatching_to(&runtime).await;
    let session = a_session(&harness).await;
    let run = over(&harness, &session).await;
    assert_eq!(run.exit, Some(Exit::Succeeded));

    harness.seal_session(session.id).await;

    archived(&run.instance.expect("an instance")).await;

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn unpublished_work_outlasts_the_idle_window_held_with_a_reason_until_released() {
    let runtime = working(&format!(
        "echo committed > kestrel/committed; git -C kestrel add committed; \
         {COMMIT} --message 'work only this instance has'; \
         echo uncommitted >> kestrel/README.md; echo untracked > kestrel/untracked"
    ));
    let harness = dispatching_to(&runtime).await;
    let session = a_session(&harness).await;
    let run = over(&harness, &session).await;
    let instance = run.instance.expect("an instance");

    harness.last_active(&session, a_day_ago()).await;
    stays_open(&harness, &session).await;

    let held = harness.held_instances("acme").await;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].session, session.id);
    assert_eq!(held[0].instance, instance);
    assert_eq!(
        held[0].because,
        format!(
            "{} on {} has 1 unpushed commit, 1 uncommitted change, 1 untracked file",
            repository::url(),
            session.checkout.branch
        )
    );
    let refused = harness
        .try_seal_session(session.id)
        .await
        .expect_err("a session whose instance holds the only copy of its work sealed");
    assert!(
        refused.to_string().contains(&held[0].because),
        "the refusal does not say what is held: {refused}"
    );
    assert!(Environment::workspace_of(&instance).is_dir());

    assert_eq!(harness.release_instance(session.id).await, instance);

    archived(&instance).await;
    assert!(harness.held_instances("acme").await.is_empty());
    assert_eq!(
        harness
            .transcript(session.id)
            .await
            .last()
            .expect("a transcript entry")
            .entry
            .to_string(),
        format!(
            "instance released  operator  {instance}  discarding {}",
            held[0].because
        )
    );
    let session_id = session.id;
    eventually("the idle sweep sealing the released session", async || {
        harness.show_session(session_id).await.state == SessionState::Sealed
    })
    .await;

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_that_fails_without_reporting_its_checkout_holds_its_instance() {
    let environment = Environment::executing("exit 3");
    let harness = Harness::dispatching(environment.path()).await;
    let session = a_session(&harness).await;
    let run = over(&harness, &session).await;
    assert!(matches!(run.exit, Some(Exit::Failed { .. })));
    let instance = run.instance.expect("an instance");

    harness.last_active(&session, a_day_ago()).await;
    stays_open(&harness, &session).await;

    let held = harness.held_instances("acme").await;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].instance, instance);
    assert_eq!(held[0].because, "no run reported what its checkout holds");
    harness
        .try_seal_session(session.id)
        .await
        .expect_err("a session whose instance nobody reported on sealed");
    assert!(Environment::workspace_of(&instance).is_dir());

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_with_no_instance_has_nothing_to_release() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;

    let refused = harness
        .try_release_instance(session.id)
        .await
        .expect_err("a session that never ran released an instance");

    assert!(refused.to_string().contains("no instance"), "{refused}");

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_waiting_between_turns_ends_when_its_clean_session_seals_idle() {
    let runtime = working("true");
    let harness = dispatching_to(&runtime).await;
    let session = a_session(&harness).await;
    let run = harness.enqueue_run(session.id).await;
    let waiting = harness.answered(run.id, 1).await;
    assert_eq!(waiting.state, RunState::Active);

    harness.last_active(&session, a_day_ago()).await;

    let session_id = session.id;
    eventually("the idle sweep sealing the session", async || {
        harness.show_session(session_id).await.state == SessionState::Sealed
    })
    .await;
    assert_eq!(harness.run(run.id).await.exit, Some(Exit::Succeeded));
    archived(&waiting.instance.expect("an instance")).await;

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_waiting_between_turns_over_unpublished_work_outlasts_the_idle_window() {
    let runtime = working("echo untracked > kestrel/untracked");
    let harness = dispatching_to(&runtime).await;
    let session = a_session(&harness).await;
    let run = harness.enqueue_run(session.id).await;
    let waiting = harness.answered(run.id, 1).await;

    harness.last_active(&session, a_day_ago()).await;
    stays_open(&harness, &session).await;

    assert_eq!(harness.run(run.id).await.state, RunState::Active);
    let held = harness.held_instances("acme").await;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].instance, waiting.instance.expect("an instance"));
    assert!(
        held[0].because.contains("1 untracked file"),
        "{}",
        held[0].because
    );

    harness.stop_run(run.id).await;
    harness.teardown().await;
}
