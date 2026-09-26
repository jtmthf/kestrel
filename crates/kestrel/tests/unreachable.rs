//! A queued Run whose declared tolerance can no longer be met becomes unreachable: terminal,
//! never claimed, and never carrying an exit status, because nothing failed. Tolerance defaults
//! to all-must-succeed, so one blocker failing is enough, however many others there are or were
//! still waiting behind it.

mod support;

use std::time::Duration;

use jiff::Timestamp;
use kestrel::domain::{Run, RunState, Session};
use support::Kestrel;

async fn a_session(kestrel: &Kestrel) -> Session {
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

    kestrel.open_session("acme", "kestrel", "builder").await
}

async fn a_dependent_blocked_on_an_active_run(kestrel: &Kestrel) -> (Run, Run) {
    let session = a_session(kestrel).await;
    let (blocker, _) = kestrel.dispatch_run(session.id).await;
    let waiting = kestrel.open_session("acme", "kestrel", "builder").await;
    let dependent = kestrel.enqueue_run(waiting.id).await;
    kestrel.block_run(&dependent, &blocker).await;

    (blocker, dependent)
}

#[tokio::test]
async fn a_run_blocked_on_a_failed_blocker_becomes_unreachable() {
    let kestrel = Kestrel::boot().await;
    let (blocker, dependent) = a_dependent_blocked_on_an_active_run(&kestrel).await;

    kestrel
        .fail_run(&blocker, "the agent could not open a pull request")
        .await;

    let dependent = kestrel.run(dependent.id).await;
    assert_eq!(dependent.state, RunState::Unreachable);
    assert!(
        dependent.exit.is_none(),
        "an unreachable run carried an exit status, and nothing failed"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn one_blocker_failing_is_enough_however_many_others_have_not_resolved() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    let (succeeds, _) = kestrel.dispatch_run(session.id).await;
    let elsewhere = kestrel.open_session("acme", "kestrel", "builder").await;
    let (fails, _) = kestrel.dispatch_run(elsewhere.id).await;
    let waiting = kestrel.open_session("acme", "kestrel", "builder").await;
    let dependent = kestrel.enqueue_run(waiting.id).await;
    kestrel.block_run(&dependent, &succeeds).await;
    kestrel.block_run(&dependent, &fails).await;

    kestrel.complete_run(&succeeds).await;
    assert_eq!(kestrel.run(dependent.id).await.state, RunState::Queued);

    kestrel
        .fail_run(&fails, "the agent could not open a pull request")
        .await;

    assert_eq!(
        kestrel.run(dependent.id).await.state,
        RunState::Unreachable,
        "a dependent with one blocker still to resolve became unreachable once another failed"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn an_unreachable_run_is_never_claimed_and_never_becomes_claimable_again() {
    let kestrel = Kestrel::boot().await;
    let (blocker, dependent) = a_dependent_blocked_on_an_active_run(&kestrel).await;
    kestrel
        .fail_run(&blocker, "the agent could not open a pull request")
        .await;
    assert_eq!(kestrel.run(dependent.id).await.state, RunState::Unreachable);

    for _ in 0..3 {
        assert!(
            kestrel.claim_run().await.is_none(),
            "an unreachable run was claimed"
        );
    }

    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        kestrel.run(dependent.id).await.state,
        RunState::Unreachable,
        "an unreachable run left its terminal state on its own"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_blocker_failed_by_its_lease_expiring_makes_its_dependent_unreachable() {
    let kestrel = Kestrel::boot().await;
    let (blocker, dependent) = a_dependent_blocked_on_an_active_run(&kestrel).await;

    kestrel.lease_until(&blocker, Timestamp::now()).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let dependent = kestrel.run(dependent.id).await;
        if dependent.state == RunState::Unreachable {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the dependent is {:?}, and its blocker's lease expiring never made it unreachable",
            dependent.state
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_blocked_on_a_run_that_just_turned_unreachable_is_unreachable_too() {
    let kestrel = Kestrel::boot().await;
    let (blocker, first) = a_dependent_blocked_on_an_active_run(&kestrel).await;
    let waiting = kestrel.open_session("acme", "kestrel", "builder").await;
    let second = kestrel.enqueue_run(waiting.id).await;
    kestrel.block_run(&second, &first).await;

    kestrel
        .fail_run(&blocker, "the agent could not open a pull request")
        .await;

    assert_eq!(kestrel.run(first.id).await.state, RunState::Unreachable);
    assert_eq!(
        kestrel.run(second.id).await.state,
        RunState::Unreachable,
        "a run blocked on one that turned unreachable never turned unreachable itself"
    );

    kestrel.teardown().await;
}
