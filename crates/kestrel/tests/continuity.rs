//! A Run whose agent process is lost goes on only where the harness can load the same
//! conversation back; otherwise it fails where it can be seen, and its Workspace and checkout wait
//! for the next Run (ADR-0024).

mod support;

use std::time::Duration;

use kestrel::domain::{Exit, RunState, Workspace, WorkspaceId, WorkspaceState};
use kestrel::log::Entry;
use kestrel_scripted_agent::conversed;
use support::environment::Environment;
use support::scripted_agent::{self, Script};
use support::{Kestrel, repository, supervisor};

const PATIENCE: Duration = Duration::from_secs(30);

async fn a_workspace(kestrel: &Kestrel) -> Workspace {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(
            &organization,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;
    kestrel
        .declare_agent(&organization, "builder", support::HARNESS, None)
        .await;
    kestrel
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;

    kestrel.open_workspace("acme", "kestrel", "builder").await
}

async fn said(kestrel: &Kestrel, workspace: WorkspaceId) -> Vec<String> {
    kestrel
        .transcript(workspace)
        .await
        .into_iter()
        .filter_map(|recorded| match recorded.entry {
            Entry::Said {
                participant,
                message,
            } if participant == "builder" => Some(message),
            _ => None,
        })
        .collect()
}

/// The scripted agent's process dies on the second prompt and a new one loads the session from
/// disk, so an answer that remembers the first prompt came from the same conversation.
#[tokio::test]
async fn an_agent_that_can_load_its_session_is_brought_back_into_the_same_conversation() {
    let kestrel = Kestrel::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Revives),
    )
    .await;
    let workspace = a_workspace(&kestrel).await;
    let run = kestrel
        .post(workspace.id, "operator", "the first thing to do")
        .await;
    kestrel.answered(run.id, 1).await;

    kestrel
        .post_while_busy(workspace.id, "operator", "the second thing to do")
        .await
        .expect("a waiting run takes the message as its next prompt");
    let answered = kestrel.answered(run.id, 2).await;

    assert_eq!(answered.state, RunState::Waiting, "{:?}", answered.exit);
    assert_eq!(kestrel.runs(workspace.id).await.len(), 1);
    let said = said(&kestrel, workspace.id).await;
    let [first, second] = said.as_slice() else {
        panic!("the agent answered other than twice, or its replay was said again: {said:?}");
    };
    assert_eq!(first, &conversed(1, &[]));
    assert!(
        second.starts_with("turn 2, after: ") && second.contains("the first thing to do"),
        "the recovered conversation does not remember the first prompt: {second}"
    );

    kestrel.stop_run(run.id).await;
    kestrel.teardown().await;
}

/// Stands in for the Harness: leaves a line in the checkout each time it starts, then
/// hands over to an agent that exits between turns and cannot load its session back.
#[cfg(unix)]
fn vanishing() -> Environment {
    Environment::executing(&format!(
        "echo started >> {}/notes\nexec {}",
        repository::NAME,
        scripted_agent::playing(Script::Vanishes)
    ))
}

#[cfg(unix)]
#[tokio::test]
async fn an_agent_lost_while_waiting_fails_the_run_and_the_next_run_takes_up_its_checkout() {
    let harness = vanishing();
    let kestrel = Kestrel::dispatching_to(
        supervisor::binary(),
        &format!("\"{}\"", harness.path().display()),
    )
    .await;
    let workspace = a_workspace(&kestrel).await;
    let lost = kestrel.post(workspace.id, "operator", "start").await;

    let deadline = tokio::time::Instant::now() + PATIENCE;
    let lost = loop {
        let run = kestrel.run(lost.id).await;
        if run.state == RunState::Ended {
            break run;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run {} outlived its agent's process",
            lost.id
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let Some(Exit::Failed { because }) = &lost.exit else {
        panic!("the run ended {:?}, and its agent vanished", lost.exit);
    };
    assert!(
        because.contains("process was lost") && because.contains("cannot resume"),
        "unhelpful exit status: {because}"
    );
    assert!(kestrel.turns(lost.id).await[0].answered_at.is_some());
    assert_eq!(
        kestrel.show_workspace(workspace.id).await.state,
        WorkspaceState::Open
    );
    let instance = lost.instance.clone().expect("an instance");
    assert_eq!(kestrel.instance(workspace.id).await, Some(instance.clone()));

    kestrel
        .post_while_busy(workspace.id, "operator", "pick it back up")
        .await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let next = loop {
        if let Some(next) = kestrel
            .runs(workspace.id)
            .await
            .into_iter()
            .find(|candidate| candidate.id != lost.id)
        {
            break next;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the next instruction never started a new run"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    kestrel.answered(next.id, 1).await;

    assert_eq!(kestrel.run(next.id).await.instance, Some(instance.clone()));
    let notes = std::fs::read_to_string(
        Environment::root_of(&instance)
            .join(repository::NAME)
            .join("notes"),
    )
    .expect("the checkout the first run left");
    assert_eq!(notes.lines().count(), 2, "{notes}");

    kestrel.teardown().await;
}
