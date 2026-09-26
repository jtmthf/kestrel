//! The idle window: a Session that finished its work and then sat idle for a day seals
//! itself, so a backlog of finished work stops piling up as Sessions nobody ever closes.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Session, SessionState};
use kestrel::log::Window;
use support::Kestrel;

const PATIENCE: Duration = Duration::from_secs(30);

/// Long enough that a Session backdated to it is inside the window whatever the sweep costs.
const WELL_INSIDE_THE_WINDOW: SignedDuration = SignedDuration::from_hours(23);

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

fn a_day_ago() -> Timestamp {
    Timestamp::now() - SignedDuration::from_hours(25)
}

async fn sealed_by_the_sweep(kestrel: &Kestrel, session: &Session) -> Session {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let shown = kestrel.show_session(session.id).await;
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
async fn stays_open(kestrel: &Kestrel, session: &Session) {
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        kestrel.show_session(session.id).await.state,
        SessionState::Open,
        "the session {} sealed itself while something was still holding it",
        session.id
    );
}

#[tokio::test]
async fn a_session_is_last_active_when_it_opens() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;

    assert_eq!(session.last_active_at, session.opened_at);
    assert_eq!(
        kestrel.show_session(session.id).await.last_active_at,
        session.opened_at
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn enqueueing_a_run_into_a_session_records_it_active() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    let backdated = a_day_ago();
    kestrel.last_active(&session, backdated).await;

    kestrel.enqueue_run(session.id).await;

    assert!(
        kestrel.show_session(session.id).await.last_active_at > backdated,
        "a session that took a run is still last active when it was backdated to"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_ending_records_its_session_active() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(session.id).await;
    let backdated = a_day_ago();
    kestrel.last_active(&session, backdated).await;

    kestrel.complete_run(&run).await;

    assert!(
        kestrel.show_session(session.id).await.last_active_at > backdated,
        "a session whose run ended is still last active when it was backdated to"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_session_whose_runs_have_all_ended_seals_itself_once_the_window_elapses() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(session.id).await;
    kestrel.complete_run(&run).await;

    kestrel.last_active(&session, a_day_ago()).await;

    let sealed = sealed_by_the_sweep(&kestrel, &session).await;
    assert!(sealed.sealed_at.is_some());

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_session_that_never_ran_anything_seals_itself_once_the_window_elapses() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;

    kestrel.last_active(&session, a_day_ago()).await;

    sealed_by_the_sweep(&kestrel, &session).await;

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_session_with_a_run_holding_its_slot_never_seals_however_old_it_is() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(session.id).await;

    kestrel.last_active(&session, a_day_ago()).await;

    stays_open(&kestrel, &session).await;

    kestrel.complete_run(&run).await;
    kestrel.last_active(&session, a_day_ago()).await;
    sealed_by_the_sweep(&kestrel, &session).await;

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_session_with_messages_waiting_on_a_busy_run_never_seals() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(session.id).await;
    // The supervisor outlives the Run, so the messages it was too busy for are still waiting
    // rather than having been handed to a Run of their own.
    kestrel.supervised(&run, "a supervisor").await;
    assert!(
        kestrel
            .post_while_busy(session.id, "jack", "one more thing")
            .await
            .is_none(),
        "a message posted while a run was busy enqueued a run of its own"
    );
    kestrel.complete_run(&run).await;

    kestrel.last_active(&session, a_day_ago()).await;

    stays_open(&kestrel, &session).await;

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_session_the_sweep_sealed_is_readable_refuses_work_and_is_never_reopened() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(session.id).await;
    kestrel.said(&run, "what it did").await;
    kestrel.complete_run(&run).await;
    let before = kestrel.transcript(session.id).await;

    kestrel.last_active(&session, a_day_ago()).await;
    sealed_by_the_sweep(&kestrel, &session).await;

    let walked: Vec<String> = kestrel
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
        kestrel.try_enqueue_run(session.id).await.is_err(),
        "a session the sweep sealed took a new run"
    );
    assert!(
        kestrel.try_start(&run).await.is_err(),
        "a session the sweep sealed took a turn"
    );
    assert!(
        kestrel
            .try_seal_session(session.id)
            .await
            .expect_err("a sealed session is never reopened")
            .to_string()
            .contains("already sealed")
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn the_window_is_a_day() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;

    kestrel
        .last_active(&session, Timestamp::now() - WELL_INSIDE_THE_WINDOW)
        .await;
    stays_open(&kestrel, &session).await;

    kestrel.last_active(&session, a_day_ago()).await;
    sealed_by_the_sweep(&kestrel, &session).await;

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_backdated_session_still_seals_after_the_control_plane_restarts() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    kestrel.last_active(&session, a_day_ago()).await;

    let kestrel = kestrel.kill_and_restart().await;

    sealed_by_the_sweep(&kestrel, &session).await;

    kestrel.teardown().await;
}
