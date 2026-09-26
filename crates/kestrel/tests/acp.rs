//! The supervisor driving a Harness over ACP (ADR-0007), against the scripted ACP agent
//! playing a canned sequence over real stdio JSON-RPC.

mod support;

use std::time::Duration;

use kestrel::domain::{Cost, Exit, Run, RunId, RunState, Usage, Workspace};
use kestrel_scripted_agent::{DEFAULT_MODEL, MUTTERED, OTHER_MODEL, OVERLONG};
use support::Kestrel;
use support::operator_log;
use support::repository;
use support::scripted_agent::{self, Script};
use support::supervisor::{self, Supervisor};

const PATIENCE: Duration = Duration::from_secs(30);
/// The Harness the fixture actually drives, so what a Run sees it advertise is recorded
/// against the name an Agent declared here names.
use support::HARNESS;

async fn a_workspace(kestrel: &Kestrel) -> Workspace {
    a_workspace_naming(kestrel, Some(OTHER_MODEL)).await
}

async fn a_workspace_naming(kestrel: &Kestrel, model: Option<&str>) -> Workspace {
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
        .declare_agent(&organization, "builder", HARNESS, model)
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

/// A Run the work role has started a supervisor for, and is therefore past reading its Agent's
/// model.
async fn in_flight(kestrel: &Kestrel, run: RunId) {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        if kestrel.run(run).await.supervisor.is_some() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run {run} never reached a supervisor"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn worked(script: Script) -> (Kestrel, Workspace, Run) {
    worked_naming(script, Some(OTHER_MODEL)).await
}

async fn worked_naming(script: Script, model: Option<&str>) -> (Kestrel, Workspace, Run) {
    let kestrel =
        Kestrel::dispatching_to(supervisor::binary(), &scripted_agent::playing(script)).await;
    let workspace = a_workspace_naming(&kestrel, model).await;
    let run = kestrel.enqueue_run(workspace.id).await;
    let ended = kestrel.after_one_turn(run.id).await;

    (kestrel, workspace, ended)
}

async fn transcript(kestrel: &Kestrel, workspace: &Workspace) -> Vec<String> {
    kestrel
        .transcript(workspace.id)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect()
}

#[tokio::test]
async fn what_the_agent_says_reaches_the_transcript_coalesced_by_the_message_it_belongs_to() {
    let (kestrel, workspace, run) = worked(Script::Speaks).await;

    assert_eq!(run.exit, Some(Exit::Succeeded));
    assert_eq!(
        transcript(&kestrel, &workspace)
            .await
            .into_iter()
            .filter(|entry| entry.starts_with("said"))
            .collect::<Vec<_>>(),
        vec![
            "said  builder  half of one message, and the other half".to_owned(),
            "said  builder  a second message".to_owned(),
        ]
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn an_agents_plan_its_tool_calls_and_its_reasoning_reach_no_transcript() {
    let (kestrel, workspace, _) = worked(Script::Speaks).await;

    let transcript = transcript(&kestrel, &workspace).await.join("\n");
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

    kestrel.teardown().await;
}

#[tokio::test]
async fn what_the_agent_writes_to_stderr_reaches_the_operator_log_mid_run_and_no_transcript() {
    let log = operator_log::capturing();
    let kestrel = Kestrel::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Mutters),
    )
    .await;
    let workspace = a_workspace(&kestrel).await;
    let run = kestrel.enqueue_run(workspace.id).await;

    let deadline = tokio::time::Instant::now() + PATIENCE;
    let relayed = loop {
        let relayed: Vec<String> = log
            .about(run.id)
            .into_iter()
            .filter(|line| line.contains("wrote to stderr"))
            .collect();
        if relayed.len() == 2 {
            break relayed;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run {run} relayed {relayed:?} of what its agent wrote",
            run = run.id
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    assert_eq!(kestrel.run(run.id).await.state, RunState::Working);
    assert!(relayed[0].contains(MUTTERED), "{}", relayed[0]);
    assert!(
        relayed[1].contains("[truncated]") && relayed[1].len() < OVERLONG,
        "the overlong line was relayed {} bytes long",
        relayed[1].len()
    );
    let transcript = transcript(&kestrel, &workspace).await.join("\n");
    assert!(!transcript.contains(MUTTERED), "{transcript}");

    kestrel.teardown().await;
}

#[tokio::test]
async fn what_the_agent_used_is_recorded_on_the_run_and_reaches_no_transcript() {
    let (kestrel, workspace, run) = worked(Script::Speaks).await;

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
    let transcript = transcript(&kestrel, &workspace).await.join("\n");
    assert!(
        !transcript.contains("1200") && !transcript.contains("0.42"),
        "the transcript carries what the agent used:\n{transcript}"
    );

    kestrel.teardown().await;
}

/// The scripted agent refuses to go on unless it is allowed, so a Run that succeeded is a
/// round-trip that completed; the supervisor says which subject it decided about.
#[tokio::test]
async fn a_permission_request_is_answered_and_the_round_trip_is_observable() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, credential) = kestrel.dispatch_run(workspace.id).await;

    let mut supervisor = Supervisor::provision(&kestrel.link(), run.id, &credential);
    supervisor.wait_until_it_says("reported connected").await;
    kestrel.start(&run).await;
    supervisor.wait_until_it_says("reported answered").await;
    kestrel.stop_run(run.id).await;

    assert!(
        supervisor.said("allowed once  tool call call-1"),
        "the supervisor never said how it answered. it said:\n{}",
        supervisor.everything_it_said()
    );
    assert_eq!(
        kestrel.run(run.id).await.exit,
        Some(Exit::Succeeded),
        "the agent was not allowed to go on"
    );

    assert!(supervisor.finishes().await.success());
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_turn_that_stops_for_any_other_reason_fails_the_run() {
    let (kestrel, _, run) = worked(Script::Refuses).await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!("the run ended {:?}, and its agent refused", run.exit);
    };
    assert!(
        because.contains("refused"),
        "unhelpful exit status: {because}"
    );

    kestrel.teardown().await;
}

/// A prompt that never became work is not an answer: a turn with no message, narration or
/// detail fails the run rather than reporting itself answered.
#[tokio::test]
async fn a_turn_that_produced_nothing_fails_the_run_and_names_why() {
    let (kestrel, _, run) = worked(Script::Silent).await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!(
            "the run ended {:?}, and its agent produced nothing",
            run.exit
        );
    };
    assert!(
        because.contains("answered the prompt with nothing"),
        "unhelpful exit status: {because}"
    );
    assert!(
        kestrel
            .turns(run.id)
            .await
            .iter()
            .all(|turn| turn.answered_at.is_none()),
        "a turn that produced nothing was recorded as answered"
    );

    kestrel.teardown().await;
}

/// Bookkeeping alone is not the agent working, but a tool call or a permission request is,
/// even when nothing is said.
#[tokio::test]
async fn a_turn_that_only_used_a_tool_or_asked_permission_is_answered() {
    for script in [Script::Works, Script::Asks] {
        let (kestrel, _, run) = worked(script).await;

        assert_eq!(run.exit, Some(Exit::Succeeded), "{script:?}");
        assert!(
            kestrel.turns(run.id).await[0].answered_at.is_some(),
            "{script:?}"
        );

        kestrel.teardown().await;
    }
}

#[tokio::test]
async fn an_agent_that_does_not_answer_acp_v1_fails_the_run_rather_than_being_prompted_anyway() {
    let (kestrel, workspace, run) = worked(Script::Predates).await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!(
            "the run ended {:?}, and its agent does not speak v1",
            run.exit
        );
    };
    assert!(because.contains("v1"), "unhelpful exit status: {because}");
    assert!(
        !transcript(&kestrel, &workspace)
            .await
            .iter()
            .any(|entry| entry.starts_with("said")),
        "an agent that was never initialized said something"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn an_agent_that_can_only_be_logged_into_at_a_terminal_fails_the_run_rather_than_hanging() {
    let (kestrel, _, run) = worked(Script::Demands).await;

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

    kestrel.teardown().await;
}

/// ACP offers a client no way to choose between the login methods an agent advertises, so the
/// one kestrel uses is configuration, and an agent that will not work without one it was not
/// given fails the Run rather than leaving it waiting at a login (ADR-0007).
#[tokio::test]
async fn an_agent_that_will_not_work_until_it_is_logged_in_fails_the_run_rather_than_hanging() {
    let (kestrel, _, run) = worked(Script::Insists).await;

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

    kestrel.teardown().await;
}

/// A model reaches the agent through `session/set_config_option` rather than through what the
/// Environment was built with. What the agent was set to is on the Run, where nothing inside the
/// Environment has to be believed.
#[tokio::test]
async fn the_supervisor_sets_the_model_it_was_given() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel).await;
    let (run, credential) = kestrel.dispatch_run(workspace.id).await;

    let mut supervisor = Supervisor::provision_selecting(
        &kestrel.link(),
        run.id,
        &credential,
        Script::Speaks,
        OTHER_MODEL,
    );
    supervisor.wait_until_it_says("reported connected").await;
    kestrel.start(&run).await;
    supervisor.wait_until_it_says("reported answered").await;
    kestrel.stop_run(run.id).await;

    let ended = kestrel.run(run.id).await;
    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert_eq!(ended.worked_model.as_deref(), Some(OTHER_MODEL));

    assert!(supervisor.finishes().await.success());
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_without_a_model_uses_its_agents_model() {
    let (kestrel, _, run) = worked_naming(Script::Speaks, Some(OTHER_MODEL)).await;

    assert!(run.model.is_none());
    assert_eq!(run.worked_model.as_deref(), Some(OTHER_MODEL));

    kestrel.teardown().await;
}

/// The harness's default is the honest answer for a Run that names no model, and a Run that
/// could not say which model that was would leave an audit record that says nothing (ADR-0007).
#[tokio::test]
async fn a_run_and_its_agent_that_name_no_model_use_the_harness_default() {
    let (kestrel, _, run) = worked_naming(Script::Speaks, None).await;

    assert_eq!(run.exit, Some(Exit::Succeeded));
    assert_eq!(run.worked_model.as_deref(), Some(DEFAULT_MODEL));

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_that_names_no_model_skips_selection_when_its_harness_offers_none() {
    let (kestrel, _, run) = worked_naming(Script::Decides, None).await;

    assert_eq!(run.exit, Some(Exit::Succeeded));
    assert!(run.worked_model.is_none());

    kestrel.teardown().await;
}

#[tokio::test]
async fn two_runs_in_one_workspace_can_drive_different_models() {
    let kestrel = Kestrel::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Speaks),
    )
    .await;
    let workspace = a_workspace_naming(&kestrel, Some(OTHER_MODEL)).await;

    let built = kestrel
        .enqueue_run_naming(workspace.id, Some(OTHER_MODEL))
        .await;
    let built = kestrel.after_one_turn(built.id).await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let reviewed = loop {
        match kestrel
            .try_enqueue_run_naming(workspace.id, Some(DEFAULT_MODEL))
            .await
        {
            Ok(run) => break run,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("the workspace never took its next run: {error}"),
        }
    };
    let reviewed = kestrel.after_one_turn(reviewed.id).await;

    assert_eq!(built.model.as_deref(), Some(OTHER_MODEL));
    assert_eq!(reviewed.model.as_deref(), Some(DEFAULT_MODEL));
    assert_eq!(built.worked_model.as_deref(), Some(OTHER_MODEL));
    assert_eq!(reviewed.worked_model.as_deref(), Some(DEFAULT_MODEL));

    kestrel.teardown().await;
}

/// An Agent's model is configuration rather than a rebuild, and a Run already in flight was
/// handed the model it was dispatched with.
#[tokio::test]
async fn changing_an_agents_model_leaves_a_run_already_in_flight_on_the_one_it_started_on() {
    let kestrel = Kestrel::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Lingers),
    )
    .await;
    let workspace = a_workspace_naming(&kestrel, Some(OTHER_MODEL)).await;
    let run = kestrel.enqueue_run(workspace.id).await;
    in_flight(&kestrel, run.id).await;

    kestrel
        .set_agent_model(&workspace.organization, "builder", Some(DEFAULT_MODEL))
        .await;
    assert_eq!(
        kestrel.run(run.id).await.state,
        RunState::Working,
        "the run was over before its agent's model changed"
    );

    assert_eq!(
        kestrel.after_one_turn(run.id).await.worked_model.as_deref(),
        Some(OTHER_MODEL)
    );

    kestrel.teardown().await;
}

/// Config options are optional and every agent ships a default, so a harness may let no client
/// choose a model at all. Running one on something other than what its Run named would leave
/// an audit record that lies, which is the worst of the three available outcomes (ADR-0007).
#[tokio::test]
async fn an_agent_that_lets_no_client_choose_a_model_fails_a_run_that_named_one() {
    let kestrel = Kestrel::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Decides),
    )
    .await;
    let workspace = a_workspace_naming(&kestrel, None).await;
    let run = kestrel
        .enqueue_run_naming(workspace.id, Some(OTHER_MODEL))
        .await;
    let run = kestrel.after_one_turn(run.id).await;

    assert_eq!(run.model.as_deref(), Some(OTHER_MODEL));

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!(
            "the run ended {:?}, and its harness offers no model to select",
            run.exit
        );
    };
    assert!(
        because.contains("this run named") && because.contains(OTHER_MODEL),
        "the run failed without explaining why its named model could not be selected: {because}"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_model_the_agent_does_not_offer_fails_the_run_rather_than_falling_back_to_a_default() {
    let (kestrel, _, run) = worked_naming(Script::Speaks, Some("a-model-no-agent-offers")).await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!(
            "the run ended {:?}, and its run named a model the harness does not offer",
            run.exit
        );
    };
    assert!(
        because.contains("a-model-no-agent-offers"),
        "unhelpful exit status: {because}"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn an_agent_that_dies_mid_turn_fails_the_run_rather_than_leaving_it_hanging() {
    let (kestrel, _, run) = worked(Script::Dies).await;

    assert!(
        matches!(run.exit, Some(Exit::Failed { .. })),
        "the run ended {:?}, and its agent died mid-turn",
        run.exit
    );

    kestrel.teardown().await;
}
