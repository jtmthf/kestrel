//! The two refusals a Workspace owes its own definition: one Run in it at a time, and a sealed
//! Workspace that accepts no more work.

mod support;

use kestrel::domain::{RunState, Workspace, WorkspaceState};
use kestrel::log::Window;
use support::Kestrel;

async fn declare_fixture(kestrel: &Kestrel) {
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
}

async fn a_workspace(kestrel: &Kestrel) -> Workspace {
    declare_fixture(kestrel).await;
    kestrel.open_workspace("acme", "kestrel", "builder").await
}

#[tokio::test]
async fn a_second_run_enqueued_in_a_workspace_that_already_has_one_is_refused() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let queued = kestrel.enqueue_run(workspace.id).await;

    let refusal = kestrel
        .try_enqueue_run(workspace.id)
        .await
        .expect_err("a workspace takes one run at a time");

    assert!(
        refusal.to_string().contains(&queued.id.to_string()),
        "the refusal does not name the run holding the slot: {refusal}"
    );
    assert_eq!(kestrel.runs(workspace.id).await.len(), 1);

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_that_is_slow_or_blocked_still_occupies_the_slot_and_nothing_else_takes_it() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (blocked, _) = kestrel.dispatch_run(workspace.id).await;

    assert_eq!(kestrel.run(blocked.id).await.state, RunState::Working);
    assert!(
        kestrel.try_enqueue_run(workspace.id).await.is_err(),
        "a workspace with a run in flight took a second one"
    );
    assert!(
        kestrel.claim_run().await.is_none(),
        "something was handed out to be dispatched while a run was in flight"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_in_one_workspace_leaves_every_other_workspace_free_to_take_one() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    kestrel.dispatch_run(workspace.id).await;
    let elsewhere = kestrel.open_workspace("acme", "kestrel", "builder").await;

    let run = kestrel.enqueue_run(elsewhere.id).await;

    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(run.id),
        "a run in another workspace was not dispatched"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_that_ended_hands_its_workspaces_slot_to_the_next_one() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (first, _) = kestrel.dispatch_run(workspace.id).await;

    kestrel.complete_run(&first).await;
    let next = kestrel.enqueue_run(workspace.id).await;

    assert_eq!(
        kestrel.claim_run().await.map(|claimed| claimed.run.id),
        Some(next.id)
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn sealing_a_workspace_with_a_run_still_in_flight_is_refused() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (in_flight, _) = kestrel.dispatch_run(workspace.id).await;

    let refusal = kestrel
        .try_seal_workspace(workspace.id)
        .await
        .expect_err("a workspace with a run in flight does not seal");

    assert!(
        refusal.to_string().contains(&in_flight.id.to_string()),
        "the refusal does not name the run still in flight: {refusal}"
    );
    assert_eq!(
        kestrel.show_workspace(workspace.id).await.state,
        WorkspaceState::Open
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_whose_runs_have_all_ended_seals() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;
    kestrel.complete_run(&run).await;

    let sealed = kestrel.seal_workspace(workspace.id).await;

    assert_eq!(sealed.state, WorkspaceState::Sealed);
    assert!(sealed.sealed_at.is_some());
    assert_eq!(
        kestrel.show_workspace(workspace.id).await.state,
        sealed.state
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_that_never_ran_anything_seals() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;

    assert_eq!(
        kestrel.seal_workspace(workspace.id).await.state,
        WorkspaceState::Sealed
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_sealed_workspace_is_fully_readable_including_its_whole_transcript() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;
    kestrel.said(&run, "what it did").await;
    kestrel.complete_run(&run).await;
    let before = kestrel.transcript(workspace.id).await;

    kestrel.seal_workspace(workspace.id).await;

    let shown = kestrel.show_workspace(workspace.id).await;
    assert_eq!(shown.organization.name, "acme");
    assert_eq!(shown.project.name, "kestrel");
    assert_eq!(shown.agent.name, "builder");
    assert_eq!(kestrel.runs(workspace.id).await.len(), 1);

    let after: Vec<String> = kestrel
        .walk(workspace.id, None, Window::of(1).expect("a window"))
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

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_sealed_workspace_refuses_a_new_run() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    kestrel.seal_workspace(workspace.id).await;

    let refusal = kestrel
        .try_enqueue_run(workspace.id)
        .await
        .expect_err("a sealed workspace takes no run");

    assert!(
        refusal.to_string().contains("sealed"),
        "unhelpful refusal: {refusal}"
    );
    assert!(kestrel.runs(workspace.id).await.is_empty());

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_sealed_workspace_refuses_a_turn() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;
    kestrel.complete_run(&run).await;
    kestrel.seal_workspace(workspace.id).await;

    let refusal = kestrel
        .try_start(&run)
        .await
        .expect_err("a sealed workspace takes no turn");

    assert!(
        refusal.to_string().contains("sealed"),
        "unhelpful refusal: {refusal}"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_sealed_workspace_refuses_a_new_transcript_entry() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;
    kestrel.complete_run(&run).await;
    kestrel.seal_workspace(workspace.id).await;
    let transcript = kestrel.transcript(workspace.id).await.len();

    let refusal = kestrel
        .try_said(&run, "one word more")
        .await
        .expect_err("a sealed workspace takes no transcript entry");

    assert!(
        refusal.to_string().contains("sealed"),
        "unhelpful refusal: {refusal}"
    );
    assert_eq!(kestrel.transcript(workspace.id).await.len(), transcript);

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_sealed_workspace_is_never_reopened() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let sealed = kestrel.seal_workspace(workspace.id).await;

    let refusal = kestrel
        .try_seal_workspace(workspace.id)
        .await
        .expect_err("a sealed workspace is never sealed a second time");

    assert!(
        refusal.to_string().contains("already sealed"),
        "unhelpful refusal: {refusal}"
    );
    let still = kestrel.show_workspace(workspace.id).await;
    assert_eq!(still.state, WorkspaceState::Sealed);
    assert_eq!(still.sealed_at, sealed.sealed_at);

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_sealed_workspace_stays_sealed_across_a_restart() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    kestrel.seal_workspace(workspace.id).await;

    let kestrel = kestrel.kill_and_restart().await;

    assert_eq!(
        kestrel.show_workspace(workspace.id).await.state,
        WorkspaceState::Sealed
    );
    assert!(kestrel.try_enqueue_run(workspace.id).await.is_err());

    kestrel.teardown().await;
}

#[tokio::test]
async fn work_that_continues_a_sealed_workspace_opens_a_new_one_that_records_it() {
    let kestrel = Kestrel::boot().await;
    let sealed = a_workspace(&kestrel).await;
    kestrel.seal_workspace(sealed.id).await;

    let continuing = kestrel
        .continue_workspace("acme", "kestrel", "builder", sealed.id)
        .await;

    assert_ne!(continuing.id, sealed.id);
    assert_eq!(continuing.state, WorkspaceState::Open);
    assert_eq!(
        kestrel.show_workspace(continuing.id).await.continues,
        Some(sealed.id),
        "the new workspace does not record the sealed one"
    );
    assert_eq!(
        kestrel.continuations(sealed.id).await,
        vec![continuing.id],
        "the sealed workspace does not read the one that continues it"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_that_continues_a_sealed_one_takes_runs_of_its_own() {
    let kestrel = Kestrel::boot().await;
    let sealed = a_workspace(&kestrel).await;
    kestrel.seal_workspace(sealed.id).await;

    let continuing = kestrel
        .continue_workspace("acme", "kestrel", "builder", sealed.id)
        .await;
    let run = kestrel.enqueue_run(continuing.id).await;

    assert_eq!(kestrel.run(run.id).await.workspace, continuing.id);
    assert!(kestrel.runs(sealed.id).await.is_empty());

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_that_is_still_open_is_continued_in_rather_than_after() {
    let kestrel = Kestrel::boot().await;
    let open = a_workspace(&kestrel).await;

    let refusal = kestrel
        .try_open_workspace("acme", "kestrel", "builder", Some(open.id))
        .await
        .expect_err("an open workspace is not continued");

    assert!(
        refusal.to_string().contains("open"),
        "unhelpful refusal: {refusal}"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_sealed_workspace_in_another_organization_is_not_continued() {
    let kestrel = Kestrel::boot().await;
    let sealed = a_workspace(&kestrel).await;
    kestrel.seal_workspace(sealed.id).await;

    let globex = kestrel.declare_organization("globex").await;
    kestrel
        .declare_project(
            &globex,
            "kestrel",
            &["https://github.com/globex/kestrel".to_owned()],
            "trunk",
        )
        .await;
    kestrel
        .declare_agent(&globex, "builder", "opencode", Some("claude-opus-5"))
        .await;

    let refusal = kestrel
        .try_open_workspace("globex", "kestrel", "builder", Some(sealed.id))
        .await
        .expect_err("a workspace in another organization is not continued");

    assert!(
        refusal
            .to_string()
            .contains("no workspace in the organization globex matches"),
        "the refusal is not scoped to the organization the invocation named: {refusal}"
    );

    kestrel.teardown().await;
}
