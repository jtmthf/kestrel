//! The idle window: a Workspace that finished its work and then sat idle for a day seals
//! itself, so a backlog of finished work stops piling up as Workspaces nobody ever closes.

mod support;

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Workspace, WorkspaceState};
use kestrel::log::Window;
use support::Kestrel;

const PATIENCE: Duration = Duration::from_secs(30);

/// Long enough that a Workspace backdated to it is inside the window whatever the sweep costs.
const WELL_INSIDE_THE_WINDOW: SignedDuration = SignedDuration::from_hours(23);

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

fn a_day_ago() -> Timestamp {
    Timestamp::now() - SignedDuration::from_hours(25)
}

async fn sealed_by_the_sweep(kestrel: &Kestrel, workspace: &Workspace) -> Workspace {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let shown = kestrel.show_workspace(workspace.id).await;
        if shown.state == WorkspaceState::Sealed {
            return shown;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the workspace {} was last active at {} and never sealed itself",
            shown.id,
            shown.last_active_at
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Long enough that a sweep that was going to seal this Workspace has run several times over.
async fn stays_open(kestrel: &Kestrel, workspace: &Workspace) {
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        kestrel.show_workspace(workspace.id).await.state,
        WorkspaceState::Open,
        "the workspace {} sealed itself while something was still holding it",
        workspace.id
    );
}

#[tokio::test]
async fn a_workspace_is_last_active_when_it_opens() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;

    assert_eq!(workspace.last_active_at, workspace.opened_at);
    assert_eq!(
        kestrel.show_workspace(workspace.id).await.last_active_at,
        workspace.opened_at
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn enqueueing_a_run_into_a_workspace_records_it_active() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let backdated = a_day_ago();
    kestrel.last_active(&workspace, backdated).await;

    kestrel.enqueue_run(workspace.id).await;

    assert!(
        kestrel.show_workspace(workspace.id).await.last_active_at > backdated,
        "a workspace that took a run is still last active when it was backdated to"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_ending_records_its_workspace_active() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;
    let backdated = a_day_ago();
    kestrel.last_active(&workspace, backdated).await;

    kestrel.complete_run(&run).await;

    assert!(
        kestrel.show_workspace(workspace.id).await.last_active_at > backdated,
        "a workspace whose run ended is still last active when it was backdated to"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_whose_runs_have_all_ended_seals_itself_once_the_window_elapses() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;
    kestrel.complete_run(&run).await;

    kestrel.last_active(&workspace, a_day_ago()).await;

    let sealed = sealed_by_the_sweep(&kestrel, &workspace).await;
    assert!(sealed.sealed_at.is_some());

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_that_never_ran_anything_seals_itself_once_the_window_elapses() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;

    kestrel.last_active(&workspace, a_day_ago()).await;

    sealed_by_the_sweep(&kestrel, &workspace).await;

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_with_a_run_holding_its_slot_never_seals_however_old_it_is() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;

    kestrel.last_active(&workspace, a_day_ago()).await;

    stays_open(&kestrel, &workspace).await;

    kestrel.complete_run(&run).await;
    kestrel.last_active(&workspace, a_day_ago()).await;
    sealed_by_the_sweep(&kestrel, &workspace).await;

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_with_messages_waiting_on_a_busy_run_never_seals() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;
    // The supervisor outlives the Run, so the messages it was too busy for are still waiting
    // rather than having been handed to a Run of their own.
    kestrel.supervised(&run, "a supervisor").await;
    assert!(
        kestrel
            .post_while_busy(workspace.id, "jack", "one more thing")
            .await
            .is_none(),
        "a message posted while a run was busy enqueued a run of its own"
    );
    kestrel.complete_run(&run).await;

    kestrel.last_active(&workspace, a_day_ago()).await;

    stays_open(&kestrel, &workspace).await;

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_the_sweep_sealed_is_readable_refuses_work_and_is_never_reopened() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;
    kestrel.said(&run, "what it did").await;
    kestrel.complete_run(&run).await;
    let before = kestrel.transcript(workspace.id).await;

    kestrel.last_active(&workspace, a_day_ago()).await;
    sealed_by_the_sweep(&kestrel, &workspace).await;

    let walked: Vec<String> = kestrel
        .walk(workspace.id, None, Window::of(1).expect("a window"))
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
        kestrel.try_enqueue_run(workspace.id).await.is_err(),
        "a workspace the sweep sealed took a new run"
    );
    assert!(
        kestrel.try_start(&run).await.is_err(),
        "a workspace the sweep sealed took a turn"
    );
    assert!(
        kestrel
            .try_seal_workspace(workspace.id)
            .await
            .expect_err("a sealed workspace is never reopened")
            .to_string()
            .contains("already sealed")
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn the_window_is_a_day() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;

    kestrel
        .last_active(&workspace, Timestamp::now() - WELL_INSIDE_THE_WINDOW)
        .await;
    stays_open(&kestrel, &workspace).await;

    kestrel.last_active(&workspace, a_day_ago()).await;
    sealed_by_the_sweep(&kestrel, &workspace).await;

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_backdated_workspace_still_seals_after_the_control_plane_restarts() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    kestrel.last_active(&workspace, a_day_ago()).await;

    let kestrel = kestrel.kill_and_restart().await;

    sealed_by_the_sweep(&kestrel, &workspace).await;

    kestrel.teardown().await;
}
