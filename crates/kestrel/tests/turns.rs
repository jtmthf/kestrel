//! A Run is one continuing ACP conversation: each follow-up is another turn of it, and only an
//! explicit stop, the Workspace sealing, or a failure ends it (ADR-0024).

mod support;

use std::time::Duration;

use kestrel::domain::{Exit, RunId, RunState, Workspace, WorkspaceId};
use kestrel::log::{Entry, Message};
use kestrel_scripted_agent::conversed;
use support::scripted_agent::{self, Script};
use support::{HARNESS, Kestrel, repository, supervisor};

const PATIENCE: Duration = Duration::from_secs(30);

async fn conversing(script: Script) -> (Kestrel, Workspace) {
    let kestrel =
        Kestrel::dispatching_to(supervisor::binary(), &scripted_agent::playing(script)).await;
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
        .declare_agent(&organization, "builder", HARNESS, None)
        .await;
    kestrel
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;
    let workspace = kestrel.open_workspace("acme", "kestrel", "builder").await;

    (kestrel, workspace)
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

async fn prompted(kestrel: &Kestrel, run: kestrel::domain::RunId) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while kestrel.turns(run).await.is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run {run} was never prompted"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The scripted agent remembers only what its own process was prompted with, so a second
/// answer naming the first prompt is one resumed that went on, not one rebuilt from a
/// Transcript.
#[tokio::test]
async fn a_follow_up_is_the_next_turn_of_the_same_agent_conversation() {
    let (kestrel, workspace) = conversing(Script::Converses).await;
    let run = kestrel
        .post(workspace.id, "operator", "the first thing to do")
        .await;
    kestrel.answered(run.id, 1).await;

    let continued = kestrel
        .post_while_busy(workspace.id, "operator", "the second thing to do")
        .await
        .expect("a waiting run takes the message as its next prompt");
    assert_eq!(continued.id, run.id);
    let answered = kestrel.answered(run.id, 2).await;

    assert_eq!(answered.state, RunState::Waiting);
    assert_eq!(kestrel.runs(workspace.id).await.len(), 1);
    assert_eq!(kestrel.turns(run.id).await.len(), 2);
    let said = said(&kestrel, workspace.id).await;
    let [first, second] = said.as_slice() else {
        panic!("the agent answered other than twice: {said:?}");
    };
    assert_eq!(first, &conversed(1, &[]));
    assert!(
        second.starts_with("turn 2, after: ") && second.contains("the first thing to do"),
        "the second answer does not remember the first prompt: {second}"
    );

    kestrel.stop_run(run.id).await;
    kestrel.teardown().await;
}

/// Nothing an agent says is how kestrel learns the work is over: "done" and a pull request are
/// both only words in an answer.
#[tokio::test]
async fn an_answer_saying_the_work_is_done_leaves_the_run_open() {
    let (kestrel, workspace) = conversing(Script::Echoes).await;
    let run = kestrel
        .post(
            workspace.id,
            "operator",
            "Done. Opened https://github.com/jtmthf/kestrel/pull/1",
        )
        .await;
    kestrel.answered(run.id, 1).await;

    kestrel
        .post_while_busy(workspace.id, "operator", "one more thing")
        .await
        .expect("the run is still open to take it");
    let answered = kestrel.answered(run.id, 2).await;

    assert_eq!(answered.state, RunState::Waiting);
    assert_eq!(said(&kestrel, workspace.id).await[1], "one more thing");

    kestrel.stop_run(run.id).await;
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_holds_one_unfinished_run_even_while_it_waits() {
    let (kestrel, workspace) = conversing(Script::Converses).await;
    let run = kestrel.post(workspace.id, "operator", "start").await;
    kestrel.answered(run.id, 1).await;

    let refused = kestrel
        .try_enqueue_run(workspace.id)
        .await
        .expect_err("a second run beside the one waiting")
        .to_string();

    assert!(refused.contains(&run.id.to_string()), "{refused}");
    kestrel.stop_run(run.id).await;
    kestrel.teardown().await;
}

#[tokio::test]
async fn stopping_a_run_while_waiting_ends_it_succeeded_and_its_supervisor_with_it() {
    let (kestrel, workspace) = conversing(Script::Converses).await;
    let run = kestrel.post(workspace.id, "operator", "start").await;
    let answered = kestrel.answered(run.id, 1).await;

    assert_eq!(kestrel.stop_run(run.id).await, Exit::Succeeded);

    let ended = kestrel.run(run.id).await;
    assert_eq!(ended.state, RunState::Ended);
    assert_eq!(ended.exit, Some(Exit::Succeeded));
    support::environment::Environment::named(answered.supervisor.as_deref().expect("a supervisor"))
        .is_gone()
        .await;
    kestrel.teardown().await;
}

#[tokio::test]
async fn stopping_a_run_mid_turn_fails_it() {
    let (kestrel, workspace) = conversing(Script::Dawdles).await;
    let run = kestrel.post(workspace.id, "operator", "start").await;
    prompted(&kestrel, run.id).await;

    let Exit::Failed { because } = kestrel.stop_run(run.id).await else {
        panic!("a run stopped before its agent answered succeeded");
    };
    assert!(because.contains("mid-turn"), "{because}");

    kestrel.teardown().await;
}

#[tokio::test]
async fn sealing_a_workspace_ends_the_run_waiting_between_its_turns() {
    let (kestrel, workspace) = conversing(Script::Converses).await;
    let run = kestrel.post(workspace.id, "operator", "start").await;
    kestrel.answered(run.id, 1).await;

    kestrel.seal_workspace(workspace.id).await;

    assert_eq!(kestrel.run(run.id).await.exit, Some(Exit::Succeeded));
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_workspace_does_not_seal_under_a_turn_in_flight() {
    let (kestrel, workspace) = conversing(Script::Dawdles).await;
    let run = kestrel.post(workspace.id, "operator", "start").await;
    prompted(&kestrel, run.id).await;

    let refused = kestrel
        .try_seal_workspace(workspace.id)
        .await
        .expect_err("a turn is in flight")
        .to_string();

    assert!(refused.contains("in flight"), "{refused}");
    kestrel.stop_run(run.id).await;
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_turn_the_agent_fails_ends_the_run() {
    let (kestrel, workspace) = conversing(Script::Refuses).await;
    let run = kestrel.post(workspace.id, "operator", "start").await;

    let ended = kestrel.answered(run.id, 1).await;

    assert!(
        matches!(ended.exit, Some(Exit::Failed { .. })),
        "the run is {:?} after its agent refused",
        ended.exit
    );
    assert!(kestrel.turns(run.id).await[0].answered_at.is_none());
    kestrel.teardown().await;
}

/// The rule is about a Turn, not only the first one: a later Turn that produces nothing fails
/// the Run too, and the next instruction starts a new Run.
#[tokio::test]
async fn a_later_turn_that_produced_nothing_fails_the_run() {
    let (kestrel, workspace) = conversing(Script::Lapses).await;
    let run = kestrel
        .post(workspace.id, "operator", "the first thing to do")
        .await;
    kestrel.answered(run.id, 1).await;

    let continued = kestrel
        .post_while_busy(workspace.id, "operator", "the second thing to do")
        .await
        .expect("a waiting run takes the message as its next prompt");
    assert_eq!(continued.id, run.id);
    let ended = kestrel.answered(run.id, 2).await;

    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and its second turn produced nothing",
            ended.exit
        );
    };
    assert!(
        because.contains("answered the prompt with nothing"),
        "unhelpful exit status: {because}"
    );
    let turns = kestrel.turns(run.id).await;
    assert_eq!(turns.len(), 2);
    assert!(
        turns[0].answered_at.is_some(),
        "the first turn was not answered"
    );
    assert!(
        turns[1].answered_at.is_none(),
        "the empty turn was recorded as answered"
    );

    // A failed Run is done: the next instruction starts a new one rather than continuing it.
    kestrel
        .post_while_busy(workspace.id, "operator", "one more try")
        .await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let next = loop {
        if let Some(next) = kestrel
            .runs(workspace.id)
            .await
            .into_iter()
            .find(|candidate| candidate.id != run.id)
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
    kestrel.stop_run(next.id).await;
    kestrel.teardown().await;
}

/// The turn in flight is not interrupted; what arrived during it is the next turn, in the
/// order it arrived.
#[tokio::test]
async fn messages_arriving_mid_turn_are_the_next_turn_of_the_same_run() {
    let (kestrel, workspace) = conversing(Script::Lingers).await;
    let run = kestrel.post(workspace.id, "operator", "start").await;
    prompted(&kestrel, run.id).await;

    for message in ["one more change", "and update the docs"] {
        assert!(
            kestrel
                .post_while_busy(workspace.id, "operator", message)
                .await
                .is_none()
        );
    }
    kestrel.answered(run.id, 2).await;

    assert_eq!(kestrel.runs(workspace.id).await.len(), 1);
    assert!(
        kestrel
            .transcript(workspace.id)
            .await
            .iter()
            .any(|recorded| {
                recorded.entry
                    == Entry::Messages {
                        messages: vec![
                            Message {
                                participant: "operator".to_owned(),
                                message: "one more change".to_owned(),
                            },
                            Message {
                                participant: "operator".to_owned(),
                                message: "and update the docs".to_owned(),
                            },
                        ],
                    }
            })
    );

    kestrel.stop_run(run.id).await;
    kestrel.teardown().await;
}

async fn sharing_one_slot() -> Kestrel {
    let kestrel = Kestrel::dispatching_harnesses_up_to(
        supervisor::binary(),
        &[
            (HARNESS, &scripted_agent::playing(Script::Converses)),
            ("claude", &scripted_agent::playing(Script::Dawdles)),
        ],
        1,
    )
    .await;
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
        .declare_agent(&organization, "builder", HARNESS, None)
        .await;
    kestrel
        .declare_agent(&organization, "dawdler", "claude", None)
        .await;
    kestrel
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;

    kestrel
}

/// Long enough that a dispatcher that was going to prompt a Run has had many chances to.
async fn not_prompted_again(kestrel: &Kestrel, run: RunId, turns: usize) {
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert_eq!(
        kestrel.turns(run).await.len(),
        turns,
        "the run {run} took a turn while another held the only slot"
    );
}

#[tokio::test]
async fn a_waiting_run_leaves_its_active_work_slot_to_another_workspace() {
    let kestrel = sharing_one_slot().await;
    let waiting = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let working = kestrel.open_workspace("acme", "kestrel", "dawdler").await;

    let first = kestrel
        .post(waiting.id, "operator", "the first thing to do")
        .await;
    kestrel.answered(first.id, 1).await;
    let second = kestrel.post(working.id, "operator", "work on").await;
    prompted(&kestrel, second.id).await;

    let continued = kestrel
        .post_while_busy(waiting.id, "operator", "the second thing to do")
        .await
        .expect("a waiting run takes the message as its next prompt");
    assert_eq!(continued.id, first.id);
    not_prompted_again(&kestrel, first.id, 1).await;
    assert!(kestrel.has_pending_messages(waiting.id).await);

    kestrel.stop_run(second.id).await;
    let answered = kestrel.answered(first.id, 2).await;

    assert_eq!(answered.state, RunState::Waiting);
    assert_eq!(kestrel.runs(waiting.id).await.len(), 1);
    let said = said(&kestrel, waiting.id).await;
    assert!(
        said.get(1)
            .is_some_and(|second| second.starts_with("turn 2, after: ")
                && second.contains("the first thing to do")),
        "the resumed run is not the same conversation: {said:?}"
    );

    kestrel.stop_run(first.id).await;
    kestrel.teardown().await;
}

/// A freed slot goes to whichever asked for it first, so a chatty conversation cannot starve a
/// Workspace that was queued before it spoke.
#[tokio::test]
async fn a_run_queued_before_a_follow_up_arrived_takes_the_freed_slot_first() {
    let kestrel = sharing_one_slot().await;
    let waiting = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let holding = kestrel.open_workspace("acme", "kestrel", "dawdler").await;
    let queued = kestrel.open_workspace("acme", "kestrel", "dawdler").await;

    let resumed = kestrel.post(waiting.id, "operator", "start").await;
    kestrel.answered(resumed.id, 1).await;
    let busy = kestrel.post(holding.id, "operator", "work on").await;
    prompted(&kestrel, busy.id).await;
    let next = kestrel.post(queued.id, "operator", "then this").await;
    kestrel
        .post_while_busy(waiting.id, "operator", "and one more thing")
        .await
        .expect("a waiting run takes the message as its next prompt");

    kestrel.stop_run(busy.id).await;
    prompted(&kestrel, next.id).await;
    not_prompted_again(&kestrel, resumed.id, 1).await;

    kestrel.stop_run(next.id).await;
    kestrel.answered(resumed.id, 2).await;
    kestrel.stop_run(resumed.id).await;
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_follow_up_held_before_a_run_was_queued_takes_the_freed_slot_first() {
    let kestrel = sharing_one_slot().await;
    let waiting = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let holding = kestrel.open_workspace("acme", "kestrel", "dawdler").await;
    let queued = kestrel.open_workspace("acme", "kestrel", "dawdler").await;

    let resumed = kestrel.post(waiting.id, "operator", "start").await;
    kestrel.answered(resumed.id, 1).await;
    let busy = kestrel.post(holding.id, "operator", "work on").await;
    prompted(&kestrel, busy.id).await;
    kestrel
        .post_while_busy(waiting.id, "operator", "and one more thing")
        .await
        .expect("a waiting run takes the message as its next prompt");
    let next = kestrel.post(queued.id, "operator", "then this").await;

    kestrel.stop_run(busy.id).await;
    kestrel.answered(resumed.id, 2).await;
    prompted(&kestrel, next.id).await;

    let answered = kestrel.turns(resumed.id).await[1]
        .answered_at
        .expect("an answered turn");
    assert!(
        kestrel.turns(next.id).await[0].prompted_at > answered,
        "the queued run took the slot before the follow-up held ahead of it had its turn"
    );

    kestrel.stop_run(next.id).await;
    kestrel.stop_run(resumed.id).await;
    kestrel.teardown().await;
}
