//! ADR-0002's definition of done for rung 0.1: the control plane dies under a Run in flight,
//! comes back, and the Run completes with a gap-free Transcript. The Environment outlives it,
//! because the supervisor holds a cursor and nothing else.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Exit, Run, RunId, RunState, Workspace};
use kestrel::link::credential::Secret;
use kestrel::work::{Report, Reported};
use reqwest::StatusCode;
use serde_json::json;
use support::Kestrel;
use support::link_client::Link;
use support::scripted_agent::Script;
use support::supervisor::Supervisor;

const PATIENCE: Duration = Duration::from_secs(30);
const LONG_ENOUGH_TO_BE_SURE: Duration = Duration::from_secs(1);

async fn a_workspace(kestrel: &Kestrel) -> Workspace {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(&organization, "kestrel", &[], "main")
        .await;
    kestrel
        .declare_agent(&organization, "builder", "opencode", Some("claude-opus-5"))
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

async fn transcript(kestrel: &Kestrel, workspace: &Workspace) -> Vec<String> {
    kestrel
        .transcript(workspace.id)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect()
}

async fn working(kestrel: &Kestrel, workspace: &Workspace, script: Script) -> (Run, Supervisor) {
    let (run, credential) = kestrel.dispatch_run(workspace.id).await;
    let mut supervisor =
        Supervisor::provision_playing(&kestrel.link(), run.id, &credential, script);

    supervisor.wait_until_it_says("reported connected").await;
    kestrel.start(&run).await;
    supervisor.wait_until_it_says("reported started").await;

    (run, supervisor)
}

/// Killed while the agent is still working at its turn, and left down until the Environment
/// has something to say and finds nothing there to say it to.
async fn killed_mid_run() -> (Kestrel, Workspace, Run, Supervisor) {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, mut supervisor) = working(&kestrel, &workspace, Script::Lingers).await;

    let stopped = kestrel.kill().await;
    supervisor.wait_until_it_says("lost the link").await;

    (stopped.restart().await, workspace, run, supervisor)
}

#[tokio::test]
async fn a_turn_in_flight_when_the_control_plane_is_killed_is_answered_after_it_restarts() {
    let (kestrel, _, run, supervisor) = killed_mid_run().await;

    let ended = kestrel.after_one_turn(run.id).await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert!(supervisor.finishes().await.success());
    kestrel.teardown().await;
}

#[tokio::test]
async fn the_transcript_of_a_run_that_outlived_a_restart_has_no_gap_and_no_duplicate() {
    let (kestrel, workspace, run, supervisor) = killed_mid_run().await;
    kestrel.after_one_turn(run.id).await;

    assert_eq!(
        transcript(&kestrel, &workspace).await,
        vec![
            "participant joined  builder".to_owned(),
            format!("run started  {}", run.id),
            "said  builder  half of one message, and the other half".to_owned(),
            "said  builder  a second message".to_owned(),
            format!("run ended  {}  succeeded", run.id),
        ]
    );
    assert_eq!(
        kestrel
            .transcript(workspace.id)
            .await
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        (1..=5).collect::<Vec<_>>()
    );

    assert!(supervisor.finishes().await.success());
    kestrel.teardown().await;
}

#[tokio::test]
async fn the_environment_comes_back_on_its_own_carrying_the_cursor_it_held() {
    let (kestrel, _, run, mut supervisor) = killed_mid_run().await;

    supervisor.wait_until_it_says("link open after").await;

    assert!(
        kestrel.run(run.id).await.connected.is_some(),
        "the control plane that came back does not know an environment is on the link"
    );
    kestrel.after_one_turn(run.id).await;
    assert!(supervisor.finishes().await.success());
    kestrel.teardown().await;
}

/// A lease due sooner than a real one, and further off than the Environment's next heartbeat.
fn shortened() -> Timestamp {
    Timestamp::now() + SignedDuration::from_secs(6)
}

/// A second Run whose lease is already up when the control plane comes back, so the sweep
/// that reaps it is observably the same sweep that left the live one alone.
#[tokio::test]
async fn a_lease_is_not_swept_while_the_environment_that_holds_it_out_is_reconnecting() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, supervisor) = working(&kestrel, &workspace, Script::Dawdles).await;
    let (abandoned, _) = kestrel
        .dispatch_run(
            kestrel
                .open_workspace("acme", "kestrel", "builder")
                .await
                .id,
        )
        .await;

    let stopped = kestrel.kill().await;
    let shortened = shortened();
    stopped.lease_until(&run, shortened).await;
    stopped
        .lease_until(&abandoned, Timestamp::now() - SignedDuration::from_secs(1))
        .await;
    let kestrel = stopped.restart().await;

    until(&kestrel, abandoned.id, "was swept", |run| {
        run.state == RunState::Ended
    })
    .await;
    assert_eq!(
        kestrel.run(run.id).await.state,
        RunState::Working,
        "the sweep that reaped the expired lease took the live one with it"
    );

    let held = until(&kestrel, run.id, "had its lease held out again", |run| {
        run.lease_expires_at > Some(shortened)
    })
    .await;
    assert_eq!(held.state, RunState::Working);

    supervisor.destroy();
    kestrel.teardown().await;
}

#[tokio::test]
async fn restarting_with_no_run_in_flight_changes_nothing() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;
    kestrel.complete_run(&run).await;
    let before = transcript(&kestrel, &workspace).await;

    let kestrel = kestrel.kill_and_restart().await;
    tokio::time::sleep(LONG_ENOUGH_TO_BE_SURE).await;

    assert_eq!(transcript(&kestrel, &workspace).await, before);
    assert_eq!(kestrel.run(run.id).await.exit, Some(Exit::Succeeded));
    assert_eq!(kestrel.runs(workspace.id).await.len(), 1);

    kestrel.teardown().await;
}

async fn a_run(kestrel: &Kestrel) -> (Run, Secret) {
    let workspace = a_workspace(kestrel).await;

    kestrel.dispatch_run(workspace.id).await
}

#[tokio::test]
async fn a_report_whose_answer_never_arrived_is_taken_once_when_it_is_sent_again() {
    let kestrel = Kestrel::boot().await;
    let (run, credential) = a_run(&kestrel).await;
    let link = Link::to(&kestrel.link());
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
        kestrel
            .transcript(run.workspace)
            .await
            .iter()
            .filter(|entry| entry
                .entry
                .to_string()
                .contains("said once, and reported twice"))
            .count(),
        1
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_report_that_skips_one_the_environment_has_yet_to_send_is_refused() {
    let kestrel = Kestrel::boot().await;
    let (run, credential) = a_run(&kestrel).await;
    let link = Link::to(&kestrel.link());

    let refused = link
        .report_body(
            run.id,
            Some(&credential),
            &json!({"kind": "said", "seq": 2, "message": "the one before this is missing"}),
        )
        .await;

    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(kestrel.transcript(run.workspace).await.len(), 1);

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_report_that_changes_the_runs_record_and_is_not_numbered_is_refused() {
    let kestrel = Kestrel::boot().await;
    let (run, credential) = a_run(&kestrel).await;
    let link = Link::to(&kestrel.link());

    let refused = link
        .report_body(
            run.id,
            Some(&credential),
            &json!({"kind": "said", "message": "unnumbered"}),
        )
        .await;

    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(kestrel.transcript(run.workspace).await.len(), 1);

    kestrel.teardown().await;
}
