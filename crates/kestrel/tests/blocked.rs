//! A queued Run declared blocked on others is skipped in the ready order until every one of
//! its blockers has ended successfully, and the runs behind it keep their turns.

mod support;

use kestrel::domain::{RunState, Session};
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

#[tokio::test]
async fn a_run_with_an_active_blocker_is_claimed_only_after_its_blocker_ends_successfully() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (blocker, _) = harness.dispatch_run(session.id).await;
    let waiting = harness.open_session("acme", "kestrel", "builder").await;
    let dependent = harness.enqueue_run(waiting.id).await;
    harness.block_run(dependent.id, blocker.id).await;

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
    harness.block_run(dependent.id, first.id).await;
    harness.block_run(dependent.id, second.id).await;

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
    harness.block_run(dependent.id, first.id).await;
    harness.block_run(dependent.id, second.id).await;

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

    let facility = harness.open_session("acme", "kestrel", "builder").await;
    let blocked = harness.enqueue_run(facility.id).await;
    let first_eligible_session = harness.open_session("acme", "kestrel", "builder").await;
    let first_eligible = harness.enqueue_run(first_eligible_session.id).await;
    let second_eligible_session = harness.open_session("acme", "kestrel", "builder").await;
    let second_eligible = harness.enqueue_run(second_eligible_session.id).await;
    harness.block_run(blocked.id, blocker.id).await;

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
    let session = a_session(&harness).await;
    let (blocker, _) = harness.dispatch_run(session.id).await;
    let waiting = harness.open_session("acme", "kestrel", "builder").await;
    let dependent = harness.enqueue_run(waiting.id).await;
    harness.block_run(dependent.id, blocker.id).await;

    harness
        .fail_run(&blocker, "the agent could not open a pull request")
        .await;

    assert!(
        harness.claim_run().await.is_none(),
        "a run blocked on a failed run was claimed"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);

    let turn = harness.open_session("acme", "kestrel", "builder").await;
    let rather_this = harness.enqueue_run(turn.id).await;
    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(rather_this.id),
        "a failed blocker let the run behind it take the next turn"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);

    harness.teardown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_with_an_unresolved_blocker_is_not_claimed_however_many_claimants_ask_at_once() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (blocker, _) = harness.dispatch_run(session.id).await;
    let waiting = harness.open_session("acme", "kestrel", "builder").await;
    let dependent = harness.enqueue_run(waiting.id).await;
    harness.block_run(dependent.id, blocker.id).await;

    let (first, second) = tokio::join!(harness.claim_run(), harness.claim_run());

    assert!(
        first.is_none(),
        "a claimant was handed a run with an unresolved blocker"
    );
    assert!(
        second.is_none(),
        "a claimant was handed a run with an unresolved blocker"
    );
    assert_eq!(harness.run(dependent.id).await.state, RunState::Queued);

    harness.teardown().await;
}
