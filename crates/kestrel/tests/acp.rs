//! The supervisor driving an Agent Runtime over ACP (ADR-0007), against the scripted ACP agent
//! playing a canned sequence over real stdio JSON-RPC.

mod support;

use std::time::Duration;

use kestrel::domain::{Cost, Exit, Run, RunId, RunState, Session, Usage};
use kestrel::link::Instruction;
use kestrel_scripted_agent::OTHER_MODEL;
use support::Harness;
use support::repository;
use support::scripted_agent::{self, Script};
use support::supervisor::{self, Supervisor};

const PATIENCE: Duration = Duration::from_secs(30);

async fn a_session(harness: &Harness) -> Session {
    a_session_naming(harness, OTHER_MODEL).await
}

async fn a_session_naming(harness: &Harness, model: &str) -> Session {
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
        .declare_agent(&organization, "builder", "opencode", model)
        .await;

    harness
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;

    harness.open_session("acme", "kestrel", "builder").await
}

async fn ended(harness: &Harness, run: RunId) -> Run {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let run = harness.run(run).await;
        if run.state == RunState::Ended {
            return run;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run {} is {} and never ended",
            run.id,
            run.state
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn worked(script: Script) -> (Harness, Session, Run) {
    worked_naming(script, OTHER_MODEL).await
}

async fn worked_naming(script: Script, model: &str) -> (Harness, Session, Run) {
    let harness =
        Harness::dispatching_to(supervisor::binary(), &scripted_agent::playing(script)).await;
    let session = a_session_naming(&harness, model).await;
    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    (harness, session, ended)
}

async fn transcript(harness: &Harness, session: &Session) -> Vec<String> {
    harness
        .transcript(session.id)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect()
}

#[tokio::test]
async fn what_the_agent_says_reaches_the_transcript_coalesced_by_the_message_it_belongs_to() {
    let (harness, session, run) = worked(Script::Speaks).await;

    assert_eq!(run.exit, Some(Exit::Succeeded));
    assert_eq!(
        transcript(&harness, &session)
            .await
            .into_iter()
            .filter(|entry| entry.starts_with("said"))
            .collect::<Vec<_>>(),
        vec![
            "said  builder  half of one message, and the other half".to_owned(),
            "said  builder  a second message".to_owned(),
        ]
    );

    harness.teardown().await;
}

#[tokio::test]
async fn an_agents_plan_its_tool_calls_and_its_reasoning_reach_no_transcript() {
    let (harness, session, _) = worked(Script::Speaks).await;

    let transcript = transcript(&harness, &session).await.join("\n");
    for inside_the_run in [
        "read the issue",
        "the issue looks small",
        "read README.md",
        "call-1",
    ] {
        assert!(
            !transcript.contains(inside_the_run),
            "the transcript carries {inside_the_run}, which happened inside the run:\n{transcript}"
        );
    }

    harness.teardown().await;
}

#[tokio::test]
async fn what_the_agent_used_is_recorded_on_the_run_and_reaches_no_transcript() {
    let (harness, session, run) = worked(Script::Speaks).await;

    assert_eq!(
        run.usage,
        Some(Usage {
            context_used: 1_200,
            context_size: 200_000,
            cost: Some(Cost {
                amount: 0.42,
                currency: "USD".to_owned(),
            }),
        })
    );
    let transcript = transcript(&harness, &session).await.join("\n");
    assert!(
        !transcript.contains("1200") && !transcript.contains("0.42"),
        "the transcript carries what the agent used:\n{transcript}"
    );

    harness.teardown().await;
}

/// The scripted agent refuses to go on unless it is allowed, so a Run that succeeded is a
/// round-trip that completed; the supervisor says which subject it decided about.
#[tokio::test]
async fn a_permission_request_is_answered_and_the_round_trip_is_observable() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, credential) = harness.dispatch_run(session.id).await;

    let mut supervisor = Supervisor::provision(&harness.link(), run.id, &credential);
    supervisor.wait_until_it_says("reported connected").await;
    harness.instruct(&run, Instruction::Start).await;
    supervisor.wait_until_it_says("reported finished").await;

    assert!(
        supervisor.said("allowed once  tool call call-1"),
        "the supervisor never said how it answered. it said:\n{}",
        supervisor.everything_it_said()
    );
    assert_eq!(
        ended(&harness, run.id).await.exit,
        Some(Exit::Succeeded),
        "the agent was not allowed to go on"
    );

    assert!(supervisor.finishes().await.success());
    harness.teardown().await;
}

#[tokio::test]
async fn a_turn_that_stops_for_any_other_reason_fails_the_run() {
    let (harness, _, run) = worked(Script::Refuses).await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!("the run ended {:?}, and its agent refused", run.exit);
    };
    assert!(
        because.contains("refused"),
        "unhelpful exit status: {because}"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn an_agent_that_does_not_answer_acp_v1_fails_the_run_rather_than_being_prompted_anyway() {
    let (harness, session, run) = worked(Script::Predates).await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!(
            "the run ended {:?}, and its agent does not speak v1",
            run.exit
        );
    };
    assert!(because.contains("v1"), "unhelpful exit status: {because}");
    assert!(
        !transcript(&harness, &session)
            .await
            .iter()
            .any(|entry| entry.starts_with("said")),
        "an agent that was never initialized said something"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn an_agent_that_can_only_be_logged_into_at_a_terminal_fails_the_run_rather_than_hanging() {
    let (harness, _, run) = worked(Script::Demands).await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!(
            "the run ended {:?}, and nobody was at a terminal to log its agent in",
            run.exit
        );
    };
    assert!(
        because.contains("terminal"),
        "unhelpful exit status: {because}"
    );

    harness.teardown().await;
}

/// ACP offers a client no way to choose between the login methods an agent advertises, so the
/// one kestrel uses is configuration, and an agent that will not work without one it was not
/// given fails the Run rather than leaving it waiting at a login (ADR-0007).
#[tokio::test]
async fn an_agent_that_will_not_work_until_it_is_logged_in_fails_the_run_rather_than_hanging() {
    let (harness, _, run) = worked(Script::Insists).await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!(
            "the run ended {:?}, and nothing had logged its agent in",
            run.exit
        );
    };
    assert!(
        because.contains("its-own"),
        "the run failed without naming what the agent offers to be logged in with: {because}"
    );

    harness.teardown().await;
}

/// A model reaches the agent through `session/set_config_option` rather than through what the
/// Environment was built with, so changing an Agent's model is configuration (ADR-0007).
#[tokio::test]
async fn the_model_a_runs_agent_named_is_the_one_the_agent_is_set_to() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, credential) = harness.dispatch_run(session.id).await;

    let mut supervisor = Supervisor::provision_selecting(
        &harness.link(),
        run.id,
        &credential,
        Script::Speaks,
        OTHER_MODEL,
    );
    supervisor.wait_until_it_says("reported connected").await;
    harness.instruct(&run, Instruction::Start).await;
    supervisor.wait_until_it_says("reported finished").await;

    assert!(
        supervisor.said(&format!("selected the model {OTHER_MODEL}")),
        "the supervisor never said which model it set. it said:\n{}",
        supervisor.everything_it_said()
    );
    assert_eq!(ended(&harness, run.id).await.exit, Some(Exit::Succeeded));

    assert!(supervisor.finishes().await.success());
    harness.teardown().await;
}

/// Config options are optional and every agent ships a default, so a runtime may let no client
/// choose a model at all. Running one on something other than what its Agent named would leave
/// an audit record that lies, which is the worst of the three available outcomes (ADR-0007).
#[tokio::test]
async fn an_agent_that_lets_no_client_choose_a_model_fails_a_run_whose_agent_named_one() {
    let (harness, _, run) = worked(Script::Decides).await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!(
            "the run ended {:?}, and its agent offers no model to select",
            run.exit
        );
    };
    assert!(
        because.contains(OTHER_MODEL),
        "the run failed without naming the model it could not have: {because}"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_model_the_agent_does_not_offer_fails_the_run_rather_than_falling_back_to_a_default() {
    let (harness, _, run) = worked_naming(Script::Speaks, "a-model-no-agent-offers").await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!(
            "the run ended {:?}, and its agent named a model the runtime does not offer",
            run.exit
        );
    };
    assert!(
        because.contains("a-model-no-agent-offers"),
        "unhelpful exit status: {because}"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn an_agent_that_dies_mid_turn_fails_the_run_rather_than_leaving_it_hanging() {
    let (harness, _, run) = worked(Script::Dies).await;

    assert!(
        matches!(run.exit, Some(Exit::Failed { .. })),
        "the run ended {:?}, and its agent died mid-turn",
        run.exit
    );

    harness.teardown().await;
}
