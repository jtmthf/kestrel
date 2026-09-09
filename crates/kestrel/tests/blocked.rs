//! A queued Run declared blocked on others is skipped in the ready order until every one of
//! its blockers has ended successfully, and the runs behind it keep their turns.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Run, RunId, RunState, Session, SessionState};
use kestrel::work::Claimed;
use support::Harness;

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
        .declare_agent(&organization, "builder", "opencode", Some("claude-opus-5"))
        .await;

    harness.open_session("acme", "kestrel", "builder").await
}

fn claimed(first: Option<Claimed>, second: Option<Claimed>) -> Vec<RunId> {
    [first, second]
        .into_iter()
        .flatten()
        .map(|claimed| claimed.run.id)
        .collect()
}

struct Blocked {
    blocker: Run,
    dependent: Run,
    waiting: Session,
}

async fn a_run_blocked_on_an_active_one(harness: &Harness) -> Blocked {
    let session = a_session(harness).await;
    let (blocker, _) = harness.dispatch_run(session.id).await;
    let waiting = harness.open_session("acme", "kestrel", "builder").await;
    let dependent = harness.enqueue_run(waiting.id).await;
    harness.block_run(&dependent, &blocker).await;

    Blocked {
        blocker,
        dependent,
        waiting,
    }
}

/// Long enough that a sweep that was going to seal this Session has run several times over.
async fn stays_open(harness: &Harness, session: &Session) {
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        harness.show_session(session.id).await.state,
        SessionState::Open,
        "the session {} sealed itself while a blocked run was still waiting in it",
        session.id
    );
}

#[tokio::test]
async fn a_run_with_an_active_blocker_is_claimed_only_after_its_blocker_ends_successfully() {
    let harness = Harness::boot().await;
    let Blocked {
        blocker, dependent, ..
    } = a_run_blocked_on_an_active_one(&harness).await;

    assert!(
        harness.claim_run().await.is_none(),
        "a run whose blocker is still active was claimed"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);

    harness.complete_run(&blocker).await;

    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(dependent.id),
        "the run did not become claimable once its blocker ended successfully"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_blocked_on_many_is_not_claimed_until_every_blocker_has_ended_successfully() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (first, _) = harness.dispatch_run(session.id).await;
    let elsewhere = harness.open_session("acme", "kestrel", "builder").await;
    let (second, _) = harness.dispatch_run(elsewhere.id).await;
    let waiting = harness.open_session("acme", "kestrel", "builder").await;
    let dependent = harness.enqueue_run(waiting.id).await;
    harness.block_run(&dependent, &first).await;
    harness.block_run(&dependent, &second).await;

    harness.complete_run(&first).await;
    assert!(
        harness.claim_run().await.is_none(),
        "a run with one of its blockers still active was claimed"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);

    harness.complete_run(&second).await;

    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(dependent.id),
        "the run did not become claimable once every blocker had ended successfully"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_blocked_on_queued_blockers_is_claimed_only_after_they_are_claimed_and_end() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let first = harness.enqueue_run(session.id).await;
    let elsewhere = harness.open_session("acme", "kestrel", "builder").await;
    let second = harness.enqueue_run(elsewhere.id).await;
    let waiting = harness.open_session("acme", "kestrel", "builder").await;
    let dependent = harness.enqueue_run(waiting.id).await;
    harness.block_run(&dependent, &first).await;
    harness.block_run(&dependent, &second).await;

    let claimed = harness
        .claim_run()
        .await
        .expect("a blocker was queued to claim");
    assert_eq!(
        claimed.run.id, first.id,
        "the dependent run was claimed before one of its blockers"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);
    harness.complete_run(&claimed.run).await;

    let claimed = harness
        .claim_run()
        .await
        .expect("a blocker was queued to claim");
    assert_eq!(
        claimed.run.id, second.id,
        "the dependent run was claimed before its last blocker"
    );
    harness.complete_run(&claimed.run).await;

    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(dependent.id),
        "the run did not become claimable once every blocker had ended successfully"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_blocked_run_enqueued_first_is_skipped_and_never_reorders_the_runs_behind_it() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (blocker, _) = harness.dispatch_run(session.id).await;

    let blocked_session = harness.open_session("acme", "kestrel", "builder").await;
    let blocked = harness.enqueue_run(blocked_session.id).await;
    let first_eligible_session = harness.open_session("acme", "kestrel", "builder").await;
    let first_eligible = harness.enqueue_run(first_eligible_session.id).await;
    let second_eligible_session = harness.open_session("acme", "kestrel", "builder").await;
    let second_eligible = harness.enqueue_run(second_eligible_session.id).await;
    harness.block_run(&blocked, &blocker).await;

    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(first_eligible.id),
        "the blocked run enqueued first was claimed before a run enqueued after it"
    );
    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(second_eligible.id),
        "an eligible run and the one after it were claimed out of order"
    );
    assert!(
        harness.claim_run().await.is_none(),
        "a claimant was handed the blocked run"
    );
    assert_eq!(harness.run(blocked.id).await.state, RunState::Queued);

    harness.complete_run(&blocker).await;

    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(blocked.id),
        "the run enqueued first did not keep its turn once its blocker ended"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_blocked_on_a_failed_blocker_is_never_claimed() {
    let harness = Harness::boot().await;
    let Blocked {
        blocker, dependent, ..
    } = a_run_blocked_on_an_active_one(&harness).await;

    harness
        .fail_run(&blocker, "the agent could not open a pull request")
        .await;

    assert!(
        harness.claim_run().await.is_none(),
        "a run blocked on a failed run was claimed"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);

    let behind = harness.open_session("acme", "kestrel", "builder").await;
    let next_in_line = harness.enqueue_run(behind.id).await;
    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(next_in_line.id),
        "a failed blocker let the run behind it take the next turn"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_blocked_on_a_blocker_that_ended_without_an_exit_is_never_claimed() {
    let harness = Harness::boot().await;
    let Blocked {
        blocker, dependent, ..
    } = a_run_blocked_on_an_active_one(&harness).await;

    harness.end_run_without_an_exit(&blocker).await;

    assert!(
        harness.claim_run().await.is_none(),
        "a run whose blocker ended without recording an exit was claimed"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);

    harness.teardown().await;
}

#[tokio::test]
async fn a_blocked_run_keeps_its_turn_however_long_it_waits() {
    let harness = Harness::boot().await;
    let Blocked {
        blocker,
        dependent,
        waiting,
    } = a_run_blocked_on_an_active_one(&harness).await;

    harness
        .last_active(&waiting, Timestamp::now() - SignedDuration::from_hours(25))
        .await;
    stays_open(&harness, &waiting).await;

    assert!(
        harness.claim_run().await.is_none(),
        "a run whose blocker is still active was claimed after waiting out the idle window"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);

    harness.complete_run(&blocker).await;

    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(dependent.id),
        "a run that waited out the idle window did not become claimable once its blocker ended"
    );

    harness.teardown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_with_an_unresolved_blocker_is_passed_over_however_many_claimants_ask_at_once() {
    let harness = Harness::boot().await;
    let Blocked { dependent, .. } = a_run_blocked_on_an_active_one(&harness).await;
    let behind = harness.open_session("acme", "kestrel", "builder").await;
    let eligible = harness.enqueue_run(behind.id).await;

    let (first, second) = tokio::join!(harness.claim_run(), harness.claim_run());

    assert_eq!(
        claimed(first, second),
        vec![eligible.id],
        "two claimants racing past a blocked run did not take the one eligible run exactly once"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);

    harness.teardown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_one_claimant_takes_a_run_whose_last_blocker_has_just_resolved() {
    let harness = Harness::boot().await;
    let Blocked {
        blocker, dependent, ..
    } = a_run_blocked_on_an_active_one(&harness).await;

    harness.complete_run(&blocker).await;

    let (first, second) = tokio::join!(harness.claim_run(), harness.claim_run());

    assert_eq!(
        claimed(first, second),
        vec![dependent.id],
        "a run whose blocker had just resolved was handed to both claimants"
    );

    harness.teardown().await;
}
