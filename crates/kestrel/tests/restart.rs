//! ADR-0002's definition of done for rung 0.1: the control plane dies under a Run in flight,
//! comes back, and the Run completes with a gap-free Transcript. The Environment outlives it,
//! because the supervisor holds a cursor and nothing else.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Exit, Run, RunId, RunState, Session};
use kestrel::link::credential::Secret;
use kestrel::link::{Instruction, Report, Reported};
use reqwest::StatusCode;
use serde_json::json;
use support::Harness;
use support::link_client::Link;
use support::scripted_agent::Script;
use support::supervisor::Supervisor;

const PATIENCE: Duration = Duration::from_secs(30);
const LONG_ENOUGH_TO_BE_SURE: Duration = Duration::from_secs(1);

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

async fn transcript(harness: &Harness, session: &Session) -> Vec<String> {
    harness
        .transcript(session.id)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect()
}

async fn working(harness: &Harness, session: &Session, script: Script) -> (Run, Supervisor) {
    let (run, credential) = harness.dispatch_run(session.id).await;
    let mut supervisor =
        Supervisor::provision_playing(&harness.link(), run.id, &credential, script);

    supervisor.wait_until_it_says("reported connected").await;
    harness.instruct(&run, Instruction::Start).await;
    supervisor.wait_until_it_says("reported started").await;

    (run, supervisor)
}

/// Killed while the agent is still working at its turn, and left down until the Environment
/// has something to say and finds nothing there to say it to.
async fn killed_mid_run() -> (Harness, Session, Run, Supervisor) {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, mut supervisor) = working(&harness, &session, Script::Lingers).await;

    let stopped = harness.kill().await;
    supervisor.wait_until_it_says("lost the link").await;

    (stopped.restart().await, session, run, supervisor)
}

#[tokio::test]
async fn a_run_in_flight_when_the_control_plane_is_killed_completes_after_it_restarts() {
    let (harness, _, run, supervisor) = killed_mid_run().await;

    let ended = until(&harness, run.id, "ended", |run| {
        run.state == RunState::Ended
    })
    .await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert!(supervisor.finishes().await.success());
    harness.teardown().await;
}

#[tokio::test]
async fn the_transcript_of_a_run_that_outlived_a_restart_has_no_gap_and_no_duplicate() {
    let (harness, session, run, supervisor) = killed_mid_run().await;
    until(&harness, run.id, "ended", |run| {
        run.state == RunState::Ended
    })
    .await;

    assert_eq!(
        transcript(&harness, &session).await,
        vec![
            "participant joined  builder".to_owned(),
            format!("run started  {}", run.id),
            "said  builder  half of one message, and the other half".to_owned(),
            "said  builder  a second message".to_owned(),
            format!("run ended  {}  succeeded", run.id),
        ]
    );
    assert_eq!(
        harness
            .transcript(session.id)
            .await
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        (1..=5).collect::<Vec<_>>()
    );

    assert!(supervisor.finishes().await.success());
    harness.teardown().await;
}

#[tokio::test]
async fn the_environment_comes_back_on_its_own_carrying_the_cursor_it_held() {
    let (harness, _, run, mut supervisor) = killed_mid_run().await;

    supervisor.wait_until_it_says("link open after").await;

    assert!(
        harness.run(run.id).await.connected.is_some(),
        "the control plane that came back does not know an environment is on the link"
    );
    assert!(supervisor.finishes().await.success());
    harness.teardown().await;
}

/// A lease due sooner than a real one, and further off than the Environment's next heartbeat.
fn shortened() -> Timestamp {
    Timestamp::now() + SignedDuration::from_secs(6)
}

#[tokio::test]
async fn a_lease_is_not_swept_while_the_environment_that_holds_it_out_is_reconnecting() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, supervisor) = working(&harness, &session, Script::Dawdles).await;

    let stopped = harness.kill().await;
    let shortened = shortened();
    stopped.lease_until(&run, shortened).await;
    let harness = stopped.restart().await;

    let held = until(&harness, run.id, "had its lease held out again", |run| {
        run.lease_expires_at > Some(shortened)
    })
    .await;
    assert_eq!(held.state, RunState::Active);

    supervisor.destroy();
    harness.teardown().await;
}

#[tokio::test]
async fn restarting_with_no_run_in_flight_changes_nothing() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;
    harness.complete_run(&run).await;
    let before = transcript(&harness, &session).await;

    let harness = harness.kill_and_restart().await;
    tokio::time::sleep(LONG_ENOUGH_TO_BE_SURE).await;

    assert_eq!(transcript(&harness, &session).await, before);
    assert_eq!(harness.run(run.id).await.exit, Some(Exit::Succeeded));
    assert_eq!(harness.runs(session.id).await.len(), 1);

    harness.teardown().await;
}

async fn a_run(harness: &Harness) -> (Run, Secret) {
    let session = a_session(harness).await;

    harness.dispatch_run(session.id).await
}

#[tokio::test]
async fn a_report_whose_answer_never_arrived_is_taken_once_when_it_is_sent_again() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;
    let link = Link::to(&harness.link());
    let said = Reported {
        seq: Some(1),
        report: Report::Said {
            message: "said once, and reported twice".to_owned(),
        },
    };

    for _ in 0..2 {
        assert_eq!(
            link.report(run.id, Some(&credential), &said).await.status(),
            StatusCode::ACCEPTED
        );
    }

    assert_eq!(
        harness
            .transcript(run.session)
            .await
            .iter()
            .filter(|entry| entry
                .entry
                .to_string()
                .contains("said once, and reported twice"))
            .count(),
        1
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_report_that_skips_one_the_environment_has_yet_to_send_is_refused() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;
    let link = Link::to(&harness.link());

    let refused = link
        .report_body(
            run.id,
            Some(&credential),
            &json!({"kind": "said", "seq": 2, "message": "the one before this is missing"}),
        )
        .await;

    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(harness.transcript(run.session).await.len(), 1);

    harness.teardown().await;
}

#[tokio::test]
async fn a_report_that_changes_the_runs_record_and_is_not_numbered_is_refused() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;
    let link = Link::to(&harness.link());

    let refused = link
        .report_body(
            run.id,
            Some(&credential),
            &json!({"kind": "said", "message": "unnumbered"}),
        )
        .await;

    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(harness.transcript(run.session).await.len(), 1);

    harness.teardown().await;
}
