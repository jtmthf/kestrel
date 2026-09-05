//! The two refusals a Session owes its own definition: one Run in it at a time, and a sealed
//! Session that accepts no more work.

mod support;

use kestrel::domain::{RunState, Session, SessionState};
use kestrel::link::Instruction;
use kestrel::log::Window;
use support::Harness;

async fn declare_fixture(harness: &Harness) {
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
}

async fn a_session(harness: &Harness) -> Session {
    declare_fixture(harness).await;
    harness.open_session("acme", "kestrel", "builder").await
}

#[tokio::test]
async fn a_second_run_enqueued_in_a_session_that_already_has_one_is_refused() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let queued = harness.enqueue_run(session.id).await;

    let refusal = harness
        .try_enqueue_run(session.id)
        .await
        .expect_err("a session takes one run at a time");

    assert!(
        refusal.to_string().contains(&queued.id.to_string()),
        "the refusal does not name the run holding the slot: {refusal}"
    );
    assert_eq!(harness.runs(session.id).await.len(), 1);

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_that_is_slow_or_blocked_still_occupies_the_slot_and_nothing_else_takes_it() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (blocked, _) = harness.dispatch_run(session.id).await;

    assert_eq!(harness.run(blocked.id).await.state, RunState::Active);
    assert!(
        harness.try_enqueue_run(session.id).await.is_err(),
        "a session with a run in flight took a second one"
    );
    assert!(
        harness.claim_run().await.is_none(),
        "something was handed out to be dispatched while a run was in flight"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_in_one_session_leaves_every_other_session_free_to_take_one() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    harness.dispatch_run(session.id).await;
    let elsewhere = harness.open_session("acme", "kestrel", "builder").await;

    let run = harness.enqueue_run(elsewhere.id).await;

    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(run.id),
        "a run in another session was not dispatched"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_that_ended_hands_its_sessions_slot_to_the_next_one() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (first, _) = harness.dispatch_run(session.id).await;

    harness.complete_run(&first).await;
    let next = harness.enqueue_run(session.id).await;

    assert_eq!(
        harness.claim_run().await.map(|claimed| claimed.run.id),
        Some(next.id)
    );

    harness.teardown().await;
}

#[tokio::test]
async fn sealing_a_session_with_a_run_still_in_flight_is_refused() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (in_flight, _) = harness.dispatch_run(session.id).await;

    let refusal = harness
        .try_seal_session(session.id)
        .await
        .expect_err("a session with a run in flight does not seal");

    assert!(
        refusal.to_string().contains(&in_flight.id.to_string()),
        "the refusal does not name the run still in flight: {refusal}"
    );
    assert_eq!(
        harness.show_session(session.id).await.state,
        SessionState::Open
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_whose_runs_have_all_ended_seals() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;
    harness.complete_run(&run).await;

    let sealed = harness.seal_session(session.id).await;

    assert_eq!(sealed.state, SessionState::Sealed);
    assert!(sealed.sealed_at.is_some());
    assert_eq!(harness.show_session(session.id).await.state, sealed.state);

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_that_never_ran_anything_seals() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;

    assert_eq!(
        harness.seal_session(session.id).await.state,
        SessionState::Sealed
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_sealed_session_is_fully_readable_including_its_whole_transcript() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;
    harness.said(&run, "what it did").await;
    harness.complete_run(&run).await;
    let before = harness.transcript(session.id).await;

    harness.seal_session(session.id).await;

    let shown = harness.show_session(session.id).await;
    assert_eq!(shown.organization.name, "acme");
    assert_eq!(shown.workspace.name, "kestrel");
    assert_eq!(shown.agent.name, "builder");
    assert_eq!(harness.runs(session.id).await.len(), 1);

    let after: Vec<String> = harness
        .walk(session.id, None, Window::of(1).expect("a window"))
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect();
    assert_eq!(
        after,
        before
            .iter()
            .map(|entry| entry.entry.to_string())
            .collect::<Vec<_>>()
    );
    assert!(after.len() > 1, "a transcript of one entry walks nothing");

    harness.teardown().await;
}

#[tokio::test]
async fn a_sealed_session_refuses_a_new_run() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    harness.seal_session(session.id).await;

    let refusal = harness
        .try_enqueue_run(session.id)
        .await
        .expect_err("a sealed session takes no run");

    assert!(
        refusal.to_string().contains("sealed"),
        "unhelpful refusal: {refusal}"
    );
    assert!(harness.runs(session.id).await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_sealed_session_refuses_a_turn() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;
    harness.complete_run(&run).await;
    harness.seal_session(session.id).await;

    let refusal = harness
        .try_instruct(&run, Instruction::Start)
        .await
        .expect_err("a sealed session takes no turn");

    assert!(
        refusal.to_string().contains("sealed"),
        "unhelpful refusal: {refusal}"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_sealed_session_refuses_a_new_transcript_entry() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;
    harness.complete_run(&run).await;
    harness.seal_session(session.id).await;
    let transcript = harness.transcript(session.id).await.len();

    let refusal = harness
        .try_said(&run, "one word more")
        .await
        .expect_err("a sealed session takes no transcript entry");

    assert!(
        refusal.to_string().contains("sealed"),
        "unhelpful refusal: {refusal}"
    );
    assert_eq!(harness.transcript(session.id).await.len(), transcript);

    harness.teardown().await;
}

#[tokio::test]
async fn a_sealed_session_is_never_reopened() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let sealed = harness.seal_session(session.id).await;

    let refusal = harness
        .try_seal_session(session.id)
        .await
        .expect_err("a sealed session is never sealed a second time");

    assert!(
        refusal.to_string().contains("already sealed"),
        "unhelpful refusal: {refusal}"
    );
    let still = harness.show_session(session.id).await;
    assert_eq!(still.state, SessionState::Sealed);
    assert_eq!(still.sealed_at, sealed.sealed_at);

    harness.teardown().await;
}

#[tokio::test]
async fn a_sealed_session_stays_sealed_across_a_restart() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    harness.seal_session(session.id).await;

    let harness = harness.kill_and_restart().await;

    assert_eq!(
        harness.show_session(session.id).await.state,
        SessionState::Sealed
    );
    assert!(harness.try_enqueue_run(session.id).await.is_err());

    harness.teardown().await;
}

#[tokio::test]
async fn work_that_continues_a_sealed_session_opens_a_new_one_that_records_it() {
    let harness = Harness::boot().await;
    let sealed = a_session(&harness).await;
    harness.seal_session(sealed.id).await;

    let continuing = harness
        .continue_session("acme", "kestrel", "builder", sealed.id)
        .await;

    assert_ne!(continuing.id, sealed.id);
    assert_eq!(continuing.state, SessionState::Open);
    assert_eq!(
        harness.show_session(continuing.id).await.continues,
        Some(sealed.id),
        "the new session does not record the sealed one"
    );
    assert_eq!(
        harness.continuations(sealed.id).await,
        vec![continuing.id],
        "the sealed session does not read the one that continues it"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_that_continues_a_sealed_one_takes_runs_of_its_own() {
    let harness = Harness::boot().await;
    let sealed = a_session(&harness).await;
    harness.seal_session(sealed.id).await;

    let continuing = harness
        .continue_session("acme", "kestrel", "builder", sealed.id)
        .await;
    let run = harness.enqueue_run(continuing.id).await;

    assert_eq!(harness.run(run.id).await.session, continuing.id);
    assert!(harness.runs(sealed.id).await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_that_is_still_open_is_continued_in_rather_than_after() {
    let harness = Harness::boot().await;
    let open = a_session(&harness).await;

    let refusal = harness
        .try_open_session("acme", "kestrel", "builder", Some(open.id))
        .await
        .expect_err("an open session is not continued");

    assert!(
        refusal.to_string().contains("open"),
        "unhelpful refusal: {refusal}"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_sealed_session_in_another_organization_is_not_continued() {
    let harness = Harness::boot().await;
    let sealed = a_session(&harness).await;
    harness.seal_session(sealed.id).await;

    let globex = harness.declare_organization("globex").await;
    harness
        .declare_workspace(
            &globex,
            "kestrel",
            &["https://github.com/globex/kestrel".to_owned()],
            "trunk",
        )
        .await;
    harness
        .declare_agent(&globex, "builder", "opencode", "claude-opus-5")
        .await;

    let refusal = harness
        .try_open_session("globex", "kestrel", "builder", Some(sealed.id))
        .await
        .expect_err("a session in another organization is not continued");

    assert!(
        refusal.to_string().contains("acme"),
        "the refusal does not name the organization the sealed session belongs to: {refusal}"
    );

    harness.teardown().await;
}
