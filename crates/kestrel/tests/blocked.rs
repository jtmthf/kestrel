//! A queued Run declared blocked on others is skipped in the ready order until every one of
//! its blockers has ended successfully, and the runs behind it keep their turns.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Run, RunId, RunState, Workspace, WorkspaceState};
use kestrel::work::Claimed;
use support::Kestrel;

async fn a_workspace(kestrel: &Kestrel) -> Workspace {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(
            &organization,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    kestrel
        .declare_agent(&organization, "builder", "opencode", Some("claude-opus-5"))
        .await;

    kestrel.open_workspace("acme", "kestrel", "builder").await
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
    waiting: Workspace,
}

async fn a_run_blocked_on_an_active_one(kestrel: &Kestrel) -> Blocked {
    let workspace = a_workspace(kestrel).await;
    let (blocker, _) = kestrel.dispatch_run(workspace.id).await;
    let waiting = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let dependent = kestrel.enqueue_run(waiting.id).await;
    kestrel.block_run(&dependent, &blocker).await;

    Blocked {
        blocker,
        dependent,
        waiting,
    }
}

/// Long enough that a sweep that was going to seal this Workspace has run several times over.
async fn stays_open(kestrel: &Kestrel, workspace: &Workspace) {
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        kestrel.show_workspace(workspace.id).await.state,
        WorkspaceState::Open,
        "the workspace {} sealed itself while a blocked run was still waiting in it",
        workspace.id
    );
}

#[tokio::test]
async fn a_run_with_an_active_blocker_is_claimed_only_after_its_blocker_ends_successfully() {
    let kestrel = Kestrel::boot().await;
    let Blocked {
        blocker, dependent, ..
    } = a_run_blocked_on_an_active_one(&kestrel).await;

    assert!(
        kestrel.claim_run().await.is_none(),
        "a run whose blocker is still active was claimed"
    );
    assert_eq!(kestrel.run(dependent.id).await.state, RunState::Queued);

    kestrel.complete_run(&blocker).await;

    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(dependent.id),
        "the run did not become claimable once its blocker ended successfully"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_blocked_on_many_is_not_claimed_until_every_blocker_has_ended_successfully() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (first, _) = kestrel.dispatch_run(workspace.id).await;
    let elsewhere = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let (second, _) = kestrel.dispatch_run(elsewhere.id).await;
    let waiting = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let dependent = kestrel.enqueue_run(waiting.id).await;
    kestrel.block_run(&dependent, &first).await;
    kestrel.block_run(&dependent, &second).await;

    kestrel.complete_run(&first).await;
    assert!(
        kestrel.claim_run().await.is_none(),
        "a run with one of its blockers still active was claimed"
    );
    assert_eq!(kestrel.run(dependent.id).await.state, RunState::Queued);

    kestrel.complete_run(&second).await;

    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(dependent.id),
        "the run did not become claimable once every blocker had ended successfully"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_blocked_on_queued_blockers_is_claimed_only_after_they_are_claimed_and_end() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let first = kestrel.enqueue_run(workspace.id).await;
    let elsewhere = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let second = kestrel.enqueue_run(elsewhere.id).await;
    let waiting = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let dependent = kestrel.enqueue_run(waiting.id).await;
    kestrel.block_run(&dependent, &first).await;
    kestrel.block_run(&dependent, &second).await;

    let claimed = kestrel
        .claim_run()
        .await
        .expect("a blocker was queued to claim");
    assert_eq!(
        claimed.run.id, first.id,
        "the dependent run was claimed before one of its blockers"
    );
    assert_eq!(kestrel.run(dependent.id).await.state, RunState::Queued);
    kestrel.complete_run(&claimed.run).await;

    let claimed = kestrel
        .claim_run()
        .await
        .expect("a blocker was queued to claim");
    assert_eq!(
        claimed.run.id, second.id,
        "the dependent run was claimed before its last blocker"
    );
    kestrel.complete_run(&claimed.run).await;

    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(dependent.id),
        "the run did not become claimable once every blocker had ended successfully"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_blocked_run_enqueued_first_is_skipped_and_never_reorders_the_runs_behind_it() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (blocker, _) = kestrel.dispatch_run(workspace.id).await;

    let blocked_workspace = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let blocked = kestrel.enqueue_run(blocked_workspace.id).await;
    let first_eligible_workspace = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let first_eligible = kestrel.enqueue_run(first_eligible_workspace.id).await;
    let second_eligible_workspace = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let second_eligible = kestrel.enqueue_run(second_eligible_workspace.id).await;
    kestrel.block_run(&blocked, &blocker).await;

    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(first_eligible.id),
        "the blocked run enqueued first was claimed before a run enqueued after it"
    );
    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(second_eligible.id),
        "an eligible run and the one after it were claimed out of order"
    );
    assert!(
        kestrel.claim_run().await.is_none(),
        "a claimant was handed the blocked run"
    );
    assert_eq!(kestrel.run(blocked.id).await.state, RunState::Queued);

    kestrel.complete_run(&blocker).await;

    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(blocked.id),
        "the run enqueued first did not keep its turn once its blocker ended"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_blocked_on_a_failed_blocker_is_never_claimed() {
    let kestrel = Kestrel::boot().await;
    let Blocked {
        blocker, dependent, ..
    } = a_run_blocked_on_an_active_one(&kestrel).await;

    kestrel
        .fail_run(&blocker, "the agent could not open a pull request")
        .await;

    assert!(
        kestrel.claim_run().await.is_none(),
        "a run blocked on a failed run was claimed"
    );
    assert_eq!(
        kestrel.run(dependent.id).await.state,
        RunState::Unreachable,
        "a failed blocker did not make its dependent unreachable"
    );

    let behind = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let next_in_line = kestrel.enqueue_run(behind.id).await;
    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(next_in_line.id),
        "a failed blocker let the run behind it take the next turn"
    );
    assert_eq!(kestrel.run(dependent.id).await.state, RunState::Unreachable);

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_blocked_on_a_blocker_that_ended_without_an_exit_is_never_claimed() {
    let kestrel = Kestrel::boot().await;
    let Blocked {
        blocker, dependent, ..
    } = a_run_blocked_on_an_active_one(&kestrel).await;

    kestrel.end_run_without_an_exit(&blocker).await;

    assert!(
        kestrel.claim_run().await.is_none(),
        "a run whose blocker ended without recording an exit was claimed"
    );
    assert_eq!(kestrel.run(dependent.id).await.state, RunState::Queued);

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_blocked_run_keeps_its_turn_however_long_it_waits() {
    let kestrel = Kestrel::boot().await;
    let Blocked {
        blocker,
        dependent,
        waiting,
    } = a_run_blocked_on_an_active_one(&kestrel).await;

    kestrel
        .last_active(&waiting, Timestamp::now() - SignedDuration::from_hours(25))
        .await;
    stays_open(&kestrel, &waiting).await;

    assert!(
        kestrel.claim_run().await.is_none(),
        "a run whose blocker is still active was claimed after waiting out the idle window"
    );
    assert_eq!(kestrel.run(dependent.id).await.state, RunState::Queued);

    kestrel.complete_run(&blocker).await;

    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(dependent.id),
        "a run that waited out the idle window did not become claimable once its blocker ended"
    );

    kestrel.teardown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_with_an_unresolved_blocker_is_passed_over_however_many_claimants_ask_at_once() {
    let kestrel = Kestrel::boot().await;
    let Blocked { dependent, .. } = a_run_blocked_on_an_active_one(&kestrel).await;
    let behind = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let eligible = kestrel.enqueue_run(behind.id).await;

    let (first, second) = tokio::join!(kestrel.claim_run(), kestrel.claim_run());

    assert_eq!(
        claimed(first, second),
        vec![eligible.id],
        "two claimants racing past a blocked run did not take the one eligible run exactly once"
    );
    assert_eq!(kestrel.run(dependent.id).await.state, RunState::Queued);

    kestrel.teardown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_one_claimant_takes_a_run_whose_last_blocker_has_just_resolved() {
    let kestrel = Kestrel::boot().await;
    let Blocked {
        blocker, dependent, ..
    } = a_run_blocked_on_an_active_one(&kestrel).await;

    kestrel.complete_run(&blocker).await;

    let (first, second) = tokio::join!(kestrel.claim_run(), kestrel.claim_run());

    assert_eq!(
        claimed(first, second),
        vec![dependent.id],
        "a run whose blocker had just resolved was handed to both claimants"
    );

    kestrel.teardown().await;
}
