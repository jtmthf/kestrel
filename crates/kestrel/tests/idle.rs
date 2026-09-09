//! The idle window: a Session that finished its work and then sat idle for a day seals
//! itself, so a backlog of finished work stops piling up as Sessions nobody ever closes.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Session, SessionState};
use kestrel::link::Instruction;
use kestrel::log::Window;
use support::Harness;

const PATIENCE: Duration = Duration::from_secs(30);

/// Long enough that a Session backdated to it is inside the window whatever the sweep costs.
const WELL_INSIDE_THE_WINDOW: SignedDuration = SignedDuration::from_hours(23);

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

fn a_day_ago() -> Timestamp {
    Timestamp::now() - SignedDuration::from_hours(25)
}

async fn sealed_by_the_sweep(harness: &Harness, session: &Session) -> Session {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let shown = harness.show_session(session.id).await;
        if shown.state == SessionState::Sealed {
            return shown;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the session {} was last active at {} and never sealed itself",
            shown.id,
            shown.last_active_at
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Long enough that a sweep that was going to seal this Session has run several times over.
async fn stays_open(harness: &Harness, session: &Session) {
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        harness.show_session(session.id).await.state,
        SessionState::Open,
        "the session {} sealed itself while something was still holding it",
        session.id
    );
}

#[tokio::test]
async fn a_session_is_last_active_when_it_opens() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;

    assert_eq!(session.last_active_at, session.opened_at);
    assert_eq!(
        harness.show_session(session.id).await.last_active_at,
        session.opened_at
    );

    harness.teardown().await;
}

#[tokio::test]
async fn enqueueing_a_run_into_a_session_records_it_active() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let backdated = a_day_ago();
    harness.last_active(&session, backdated).await;

    harness.enqueue_run(session.id).await;

    assert!(
        harness.show_session(session.id).await.last_active_at > backdated,
        "a session that took a run is still last active when it was backdated to"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_ending_records_its_session_active() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;
    let backdated = a_day_ago();
    harness.last_active(&session, backdated).await;

    harness.complete_run(&run).await;

    assert!(
        harness.show_session(session.id).await.last_active_at > backdated,
        "a session whose run ended is still last active when it was backdated to"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_whose_runs_have_all_ended_seals_itself_once_the_window_elapses() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;
    harness.complete_run(&run).await;

    harness.last_active(&session, a_day_ago()).await;

    let sealed = sealed_by_the_sweep(&harness, &session).await;
    assert!(sealed.sealed_at.is_some());

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_that_never_ran_anything_seals_itself_once_the_window_elapses() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;

    harness.last_active(&session, a_day_ago()).await;

    sealed_by_the_sweep(&harness, &session).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_with_a_run_holding_its_slot_never_seals_however_old_it_is() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;

    harness.last_active(&session, a_day_ago()).await;

    stays_open(&harness, &session).await;

    harness.complete_run(&run).await;
    harness.last_active(&session, a_day_ago()).await;
    sealed_by_the_sweep(&harness, &session).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_with_messages_waiting_on_a_busy_run_never_seals() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;
    // The Environment outlives the Run, so the messages it was too busy for are still
    // waiting rather than having been handed to a Run of their own.
    harness.environment_present(&run, "an environment").await;
    assert!(
        harness
            .post_while_busy(session.id, "jack", "one more thing")
            .await
            .is_none(),
        "a message posted while a run was busy enqueued a run of its own"
    );
    harness.complete_run(&run).await;

    harness.last_active(&session, a_day_ago()).await;

    stays_open(&harness, &session).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_the_sweep_sealed_is_readable_refuses_work_and_is_never_reopened() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;
    harness.said(&run, "what it did").await;
    harness.complete_run(&run).await;
    let before = harness.transcript(session.id).await;

    harness.last_active(&session, a_day_ago()).await;
    sealed_by_the_sweep(&harness, &session).await;

    let walked: Vec<String> = harness
        .walk(session.id, None, Window::of(1).expect("a window"))
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect();
    assert_eq!(
        walked,
        before
            .iter()
            .map(|entry| entry.entry.to_string())
            .collect::<Vec<_>>()
    );
    assert!(
        harness.try_enqueue_run(session.id).await.is_err(),
        "a session the sweep sealed took a new run"
    );
    assert!(
        harness
            .try_instruct(&run, Instruction::Start)
            .await
            .is_err(),
        "a session the sweep sealed took a turn"
    );
    assert!(
        harness
            .try_seal_session(session.id)
            .await
            .expect_err("a sealed session is never reopened")
            .to_string()
            .contains("already sealed")
    );

    harness.teardown().await;
}

#[tokio::test]
async fn the_window_is_a_day() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;

    harness
        .last_active(&session, Timestamp::now() - WELL_INSIDE_THE_WINDOW)
        .await;
    stays_open(&harness, &session).await;

    harness.last_active(&session, a_day_ago()).await;
    sealed_by_the_sweep(&harness, &session).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_backdated_session_still_seals_after_the_control_plane_restarts() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    harness.last_active(&session, a_day_ago()).await;

    let harness = harness.kill_and_restart().await;

    sealed_by_the_sweep(&harness, &session).await;

    harness.teardown().await;
}
