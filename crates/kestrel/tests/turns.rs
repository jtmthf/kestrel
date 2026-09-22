//! A Run is one continuing ACP conversation: each follow-up is another turn of it, and only an
//! explicit stop, the Session sealing, or a failure ends it (ADR-0024).

mod support;

use std::time::Duration;

use kestrel::domain::{Exit, RunId, RunState, Session, SessionId};
use kestrel::log::{Entry, Message};
use kestrel_scripted_agent::conversed;
use support::scripted_agent::{self, Script};
use support::{Harness, RUNTIME, repository, supervisor};

const PATIENCE: Duration = Duration::from_secs(30);

async fn conversing(script: Script) -> (Harness, Session) {
    let harness =
        Harness::dispatching_to(supervisor::binary(), &scripted_agent::playing(script)).await;
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &organization,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;
    harness
        .declare_agent(&organization, "builder", RUNTIME, None)
        .await;
    harness
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;
    let session = harness.open_session("acme", "kestrel", "builder").await;

    (harness, session)
}

async fn said(harness: &Harness, session: SessionId) -> Vec<String> {
    harness
        .transcript(session)
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

async fn prompted(harness: &Harness, run: kestrel::domain::RunId) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while harness.turns(run).await.is_empty() {
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
    let (harness, session) = conversing(Script::Converses).await;
    let run = harness
        .post(session.id, "operator", "the first thing to do")
        .await;
    harness.answered(run.id, 1).await;

    let continued = harness
        .post_while_busy(session.id, "operator", "the second thing to do")
        .await
        .expect("a run between turns takes the message as its next prompt");
    assert_eq!(continued.id, run.id);
    let answered = harness.answered(run.id, 2).await;

    assert_eq!(answered.state, RunState::Active);
    assert_eq!(harness.runs(session.id).await.len(), 1);
    assert_eq!(harness.turns(run.id).await.len(), 2);
    let said = said(&harness, session.id).await;
    let [first, second] = said.as_slice() else {
        panic!("the agent answered other than twice: {said:?}");
    };
    assert_eq!(first, &conversed(1, &[]));
    assert!(
        second.starts_with("turn 2, after: ") && second.contains("the first thing to do"),
        "the second answer does not remember the first prompt: {second}"
    );

    harness.stop_run(run.id).await;
    harness.teardown().await;
}

/// Nothing an agent says is how kestrel learns the work is over: "done" and a pull request are
/// both only words in an answer.
#[tokio::test]
async fn an_answer_saying_the_work_is_done_leaves_the_run_open() {
    let (harness, session) = conversing(Script::Echoes).await;
    let run = harness
        .post(
            session.id,
            "operator",
            "Done. Opened https://github.com/jtmthf/kestrel/pull/1",
        )
        .await;
    harness.answered(run.id, 1).await;

    harness
        .post_while_busy(session.id, "operator", "one more thing")
        .await
        .expect("the run is still open to take it");
    let answered = harness.answered(run.id, 2).await;

    assert_eq!(answered.state, RunState::Active);
    assert_eq!(said(&harness, session.id).await[1], "one more thing");

    harness.stop_run(run.id).await;
    harness.teardown().await;
}

#[tokio::test]
async fn a_session_holds_one_open_run_even_while_it_waits_between_turns() {
    let (harness, session) = conversing(Script::Converses).await;
    let run = harness.post(session.id, "operator", "start").await;
    harness.answered(run.id, 1).await;

    let refused = harness
        .try_enqueue_run(session.id)
        .await
        .expect_err("a second run beside the one waiting")
        .to_string();

    assert!(refused.contains(&run.id.to_string()), "{refused}");
    harness.stop_run(run.id).await;
    harness.teardown().await;
}

#[tokio::test]
async fn stopping_a_run_between_turns_ends_it_succeeded_and_its_supervisor_with_it() {
    let (harness, session) = conversing(Script::Converses).await;
    let run = harness.post(session.id, "operator", "start").await;
    let answered = harness.answered(run.id, 1).await;

    assert_eq!(harness.stop_run(run.id).await, Exit::Succeeded);

    let ended = harness.run(run.id).await;
    assert_eq!(ended.state, RunState::Ended);
    assert_eq!(ended.exit, Some(Exit::Succeeded));
    support::environment::Environment::named(answered.supervisor.as_deref().expect("a supervisor"))
        .is_gone()
        .await;
    harness.teardown().await;
}

#[tokio::test]
async fn stopping_a_run_mid_turn_fails_it() {
    let (harness, session) = conversing(Script::Dawdles).await;
    let run = harness.post(session.id, "operator", "start").await;
    prompted(&harness, run.id).await;

    let Exit::Failed { because } = harness.stop_run(run.id).await else {
        panic!("a run stopped before its agent answered succeeded");
    };
    assert!(because.contains("mid-turn"), "{because}");

    harness.teardown().await;
}

#[tokio::test]
async fn sealing_a_session_ends_the_run_waiting_between_its_turns() {
    let (harness, session) = conversing(Script::Converses).await;
    let run = harness.post(session.id, "operator", "start").await;
    harness.answered(run.id, 1).await;

    harness.seal_session(session.id).await;

    assert_eq!(harness.run(run.id).await.exit, Some(Exit::Succeeded));
    harness.teardown().await;
}

#[tokio::test]
async fn a_session_does_not_seal_under_a_turn_in_flight() {
    let (harness, session) = conversing(Script::Dawdles).await;
    let run = harness.post(session.id, "operator", "start").await;
    prompted(&harness, run.id).await;

    let refused = harness
        .try_seal_session(session.id)
        .await
        .expect_err("a turn is in flight")
        .to_string();

    assert!(refused.contains("in flight"), "{refused}");
    harness.stop_run(run.id).await;
    harness.teardown().await;
}

#[tokio::test]
async fn a_turn_the_agent_fails_ends_the_run() {
    let (harness, session) = conversing(Script::Refuses).await;
    let run = harness.post(session.id, "operator", "start").await;

    let ended = harness.answered(run.id, 1).await;

    assert!(
        matches!(ended.exit, Some(Exit::Failed { .. })),
        "the run is {:?} after its agent refused",
        ended.exit
    );
    assert!(harness.turns(run.id).await[0].answered_at.is_none());
    harness.teardown().await;
}

/// The rule is about a Turn, not only the first one: a later Turn that produces nothing fails
/// the Run too, and the next instruction starts a new Run.
#[tokio::test]
async fn a_later_turn_that_produced_nothing_fails_the_run() {
    let (harness, session) = conversing(Script::Lapses).await;
    let run = harness
        .post(session.id, "operator", "the first thing to do")
        .await;
    harness.answered(run.id, 1).await;

    let continued = harness
        .post_while_busy(session.id, "operator", "the second thing to do")
        .await
        .expect("a run between turns takes the message as its next prompt");
    assert_eq!(continued.id, run.id);
    let ended = harness.answered(run.id, 2).await;

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
    let turns = harness.turns(run.id).await;
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
    harness
        .post_while_busy(session.id, "operator", "one more try")
        .await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let next = loop {
        if let Some(next) = harness
            .runs(session.id)
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
    harness.answered(next.id, 1).await;
    harness.stop_run(next.id).await;
    harness.teardown().await;
}

/// The turn in flight is not interrupted; what arrived during it is the next turn, in the
/// order it arrived.
#[tokio::test]
async fn messages_arriving_mid_turn_are_the_next_turn_of_the_same_run() {
    let (harness, session) = conversing(Script::Lingers).await;
    let run = harness.post(session.id, "operator", "start").await;
    prompted(&harness, run.id).await;

    for message in ["one more change", "and update the docs"] {
        assert!(
            harness
                .post_while_busy(session.id, "operator", message)
                .await
                .is_none()
        );
    }
    harness.answered(run.id, 2).await;

    assert_eq!(harness.runs(session.id).await.len(), 1);
    assert!(harness.transcript(session.id).await.iter().any(|recorded| {
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
    }));

    harness.stop_run(run.id).await;
    harness.teardown().await;
}

async fn sharing_one_slot() -> Harness {
    let harness = Harness::dispatching_runtimes_up_to(
        supervisor::binary(),
        &[
            (RUNTIME, &scripted_agent::playing(Script::Converses)),
            ("claude", &scripted_agent::playing(Script::Dawdles)),
        ],
        1,
    )
    .await;
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &organization,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;
    harness
        .declare_agent(&organization, "builder", RUNTIME, None)
        .await;
    harness
        .declare_agent(&organization, "dawdler", "claude", None)
        .await;
    harness
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;

    harness
}

/// Long enough that a dispatcher that was going to prompt a Run has had many chances to.
async fn not_prompted_again(harness: &Harness, run: RunId, turns: usize) {
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert_eq!(
        harness.turns(run).await.len(),
        turns,
        "the run {run} took a turn while another held the only slot"
    );
}

#[tokio::test]
async fn a_run_waiting_between_turns_leaves_its_slot_to_another_session() {
    let harness = sharing_one_slot().await;
    let waiting = harness.open_session("acme", "kestrel", "builder").await;
    let working = harness.open_session("acme", "kestrel", "dawdler").await;

    let first = harness
        .post(waiting.id, "operator", "the first thing to do")
        .await;
    harness.answered(first.id, 1).await;
    let second = harness.post(working.id, "operator", "work on").await;
    prompted(&harness, second.id).await;

    let continued = harness
        .post_while_busy(waiting.id, "operator", "the second thing to do")
        .await
        .expect("a run between turns takes the message as its next prompt");
    assert_eq!(continued.id, first.id);
    not_prompted_again(&harness, first.id, 1).await;
    assert!(harness.has_pending_messages(waiting.id).await);

    harness.stop_run(second.id).await;
    let answered = harness.answered(first.id, 2).await;

    assert_eq!(answered.state, RunState::Active);
    assert_eq!(harness.runs(waiting.id).await.len(), 1);
    let said = said(&harness, waiting.id).await;
    assert!(
        said.get(1)
            .is_some_and(|second| second.starts_with("turn 2, after: ")
                && second.contains("the first thing to do")),
        "the resumed run is not the same conversation: {said:?}"
    );

    harness.stop_run(first.id).await;
    harness.teardown().await;
}

/// A freed slot goes to whichever asked for it first, so a chatty conversation cannot starve a
/// Session that was queued before it spoke.
#[tokio::test]
async fn a_run_queued_before_a_follow_up_arrived_takes_the_freed_slot_first() {
    let harness = sharing_one_slot().await;
    let waiting = harness.open_session("acme", "kestrel", "builder").await;
    let holding = harness.open_session("acme", "kestrel", "dawdler").await;
    let queued = harness.open_session("acme", "kestrel", "dawdler").await;

    let resumed = harness.post(waiting.id, "operator", "start").await;
    harness.answered(resumed.id, 1).await;
    let busy = harness.post(holding.id, "operator", "work on").await;
    prompted(&harness, busy.id).await;
    let next = harness.post(queued.id, "operator", "then this").await;
    harness
        .post_while_busy(waiting.id, "operator", "and one more thing")
        .await
        .expect("a run between turns takes the message as its next prompt");

    harness.stop_run(busy.id).await;
    prompted(&harness, next.id).await;
    not_prompted_again(&harness, resumed.id, 1).await;

    harness.stop_run(next.id).await;
    harness.answered(resumed.id, 2).await;
    harness.stop_run(resumed.id).await;
    harness.teardown().await;
}

#[tokio::test]
async fn a_follow_up_held_before_a_run_was_queued_takes_the_freed_slot_first() {
    let harness = sharing_one_slot().await;
    let waiting = harness.open_session("acme", "kestrel", "builder").await;
    let holding = harness.open_session("acme", "kestrel", "dawdler").await;
    let queued = harness.open_session("acme", "kestrel", "dawdler").await;

    let resumed = harness.post(waiting.id, "operator", "start").await;
    harness.answered(resumed.id, 1).await;
    let busy = harness.post(holding.id, "operator", "work on").await;
    prompted(&harness, busy.id).await;
    harness
        .post_while_busy(waiting.id, "operator", "and one more thing")
        .await
        .expect("a run between turns takes the message as its next prompt");
    let next = harness.post(queued.id, "operator", "then this").await;

    harness.stop_run(busy.id).await;
    harness.answered(resumed.id, 2).await;
    prompted(&harness, next.id).await;

    let answered = harness.turns(resumed.id).await[1]
        .answered_at
        .expect("an answered turn");
    assert!(
        harness.turns(next.id).await[0].prompted_at > answered,
        "the queued run took the slot before the follow-up held ahead of it had its turn"
    );

    harness.stop_run(next.id).await;
    harness.stop_run(resumed.id).await;
    harness.teardown().await;
}
