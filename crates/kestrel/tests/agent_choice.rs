//! A Session starts with its Trigger's Agent, or the one an `agent:<name>` label chooses from
//! those the Trigger allows, and keeps that Agent's runtime and model for as long as it is open.

mod support;

use std::time::Duration;

use jiff::SignedDuration;
use kestrel::domain::{Direction, Exit, Run, RunId, RunState, Session};
use kestrel_scripted_agent::{DEFAULT_MODEL, OTHER_MODEL};
use serde_json::Value;
use support::github_stub::{self, GithubStub};
use support::scripted_agent::{self, Script};
use support::{Harness, client, repository, supervisor};

const PATIENCE: Duration = Duration::from_secs(30);
const REPOSITORY: &str = "jtmthf/kestrel";
const READY: &str = "ready-for-agent";
const BOTH: &[Direction] = &[Direction::Inbound, Direction::Outbound];

/// `builder` on the default runtime, and `codex` and `claude` on runtimes of their own, each
/// naming a model so a test can tell whose was recorded.
async fn an_organization(harness: &Harness) {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &organization,
            "kestrel",
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;
    harness
        .declare_agent(
            &organization,
            "builder",
            support::RUNTIME,
            Some(DEFAULT_MODEL),
        )
        .await;
    harness
        .declare_agent(&organization, "codex", "codex", Some(OTHER_MODEL))
        .await;
    harness
        .declare_agent(&organization, "claude", "claude", None)
        .await;
    harness
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;
}

async fn watching(harness: &Harness, stub: &GithubStub) {
    harness
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            BOTH,
            SignedDuration::from_millis(1),
        )
        .await;
}

/// A labelled issue carrying `labels`, fired on by a Trigger that starts `builder` and allows
/// `codex`.
async fn labelled(harness: &Harness, labels: &[&str]) -> GithubStub {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled_carrying(
        7, 43, READY, labels,
    )]));
    harness
        .declare_trigger_allowing("acme", REPOSITORY, "builder", &["codex"], None)
        .await;
    watching(harness, &stub).await;
    stub
}

async fn opened(harness: &Harness) -> Session {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        if let Some(session) = harness.sessions("acme").await.into_iter().next() {
            return harness.show_session(session.id).await;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no session was ever opened"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The reason the Trigger's firing gave for starting nothing, as a Client is shown it.
async fn refused(harness: &Harness) -> String {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        if let Some(event) = harness.events("acme").await.first() {
            let record = event.record_id.to_string();
            let operator = harness.operator();
            let shown = tokio::task::spawn_blocking(move || {
                client::ran(&operator, &["event", "show", &record])
            })
            .await
            .expect("the client should run");
            assert!(shown.status.success(), "{}", shown.err);
            let shown: Value = serde_json::from_str(&shown.out[0]).expect("a record");

            if let Some(firing) = shown["firings"].as_array().and_then(|all| all.first()) {
                assert_eq!(firing["outcome"], "failed", "{firing}");
                assert!(
                    harness.sessions("acme").await.is_empty(),
                    "a failed firing opened a session"
                );
                return firing["failure"].as_str().expect("a reason").to_owned();
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the event never fired"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
            "the run {} never ended",
            run.id
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn with_no_agent_label_a_session_starts_with_the_triggers_agent() {
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    let _github = labelled(&harness, &["bug"]).await;

    let session = opened(&harness).await;

    assert_eq!(session.agent.name, "builder");
    assert_eq!(session.agent.runtime, support::RUNTIME);
    assert_eq!(session.agent.model.as_deref(), Some(DEFAULT_MODEL));

    harness.teardown().await;
}

#[tokio::test]
async fn one_agent_label_chooses_an_agent_the_trigger_allows() {
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    let _github = labelled(&harness, &["agent:codex"]).await;

    let session = opened(&harness).await;

    assert_eq!(session.agent.name, "codex");
    assert_eq!(session.agent.runtime, "codex");
    assert_eq!(session.agent.model.as_deref(), Some(OTHER_MODEL));

    harness.teardown().await;
}

#[tokio::test]
async fn a_label_choosing_an_agent_the_trigger_does_not_allow_starts_nothing_and_says_why() {
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    let _github = labelled(&harness, &["agent:claude"]).await;

    assert_eq!(
        refused(&harness).await,
        "the label agent:claude chooses an agent the trigger ready does not allow"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn labels_choosing_two_agents_start_nothing_and_say_why() {
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    let _github = labelled(&harness, &["agent:codex", "agent:builder"]).await;

    assert_eq!(
        refused(&harness).await,
        "the labels agent:builder and agent:codex each choose an agent, and the trigger ready \
         will not guess which"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_label_on_work_that_feeds_an_open_session_changes_nothing_about_its_agent() {
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[
        github_stub::labelled_carrying(7, 43, READY, &["agent:codex"]),
        github_stub::labelled_carrying(8, 43, READY, &["agent:codex", "agent:builder"]),
    ]));
    harness
        .declare_trigger_allowing(
            "acme",
            REPOSITORY,
            "builder",
            &["codex"],
            Some("{{ event.source }}{{ event.subject }}"),
        )
        .await;
    watching(&harness, &stub).await;
    let session = opened(&harness).await;

    let deadline = tokio::time::Instant::now() + PATIENCE;
    while harness.transcript(session.id).await.len() < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the second event never fed the session"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(harness.sessions("acme").await.len(), 1);
    assert_eq!(harness.show_session(session.id).await.agent.name, "codex");

    harness.teardown().await;
}

#[tokio::test]
async fn an_open_sessions_agent_keeps_the_runtime_and_model_it_opened_with() {
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    let session = harness.open_session("acme", "kestrel", "codex").await;
    let organization = &session.organization;

    harness
        .declare_agent(organization, "codex", support::RUNTIME, None)
        .await;
    harness
        .set_agent_model(organization, "codex", Some(DEFAULT_MODEL))
        .await;

    let shown = harness.show_session(session.id).await;
    assert_eq!(shown.agent.runtime, "codex");
    assert_eq!(shown.agent.model.as_deref(), Some(OTHER_MODEL));
    let later = harness.open_session("acme", "kestrel", "codex").await;
    assert_eq!(later.agent.runtime, support::RUNTIME);
    assert_eq!(later.agent.model.as_deref(), Some(DEFAULT_MODEL));

    harness.teardown().await;
}

/// The default runtime dies, so only the runtime the label chose can end the Run well, and only
/// on the model the chosen Agent named.
#[tokio::test]
async fn the_work_role_runs_the_runtime_and_model_a_label_chose() {
    let harness = Harness::dispatching_runtimes(
        supervisor::binary(),
        &[
            (support::RUNTIME, &scripted_agent::playing(Script::Dies)),
            ("codex", &scripted_agent::playing(Script::Speaks)),
        ],
    )
    .await;
    an_organization(&harness).await;
    let _github = labelled(&harness, &["agent:codex"]).await;
    let session = opened(&harness).await;
    let organization = &session.organization;
    harness
        .set_agent_model(organization, "codex", Some(DEFAULT_MODEL))
        .await;

    let run = harness.runs(session.id).await.remove(0);
    let run = ended(&harness, run.id).await;

    assert_eq!(run.exit, Some(Exit::Succeeded), "{:?}", run.exit);
    assert_eq!(run.worked_model.as_deref(), Some(OTHER_MODEL));

    harness.teardown().await;
}

#[tokio::test]
async fn a_runtime_the_work_role_cannot_spawn_fails_the_run_and_says_which() {
    let harness = Harness::dispatching(supervisor::binary()).await;
    an_organization(&harness).await;
    let session = harness.open_session("acme", "kestrel", "claude").await;
    let run = harness.enqueue_run(session.id).await;

    let run = ended(&harness, run.id).await;

    assert_eq!(
        run.exit,
        Some(Exit::Failed {
            because: "this work role spawns no agent runtime named claude".to_owned()
        })
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_declaration_file_names_the_agents_a_label_may_choose() {
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    let file = |allows: &str| {
        format!(
            "triggers:\n  ready:\n    filter: {}\n    brief: Work\n    workspace: kestrel\n    \
             agent: builder\n{allows}",
            support::labelled_on(REPOSITORY, READY)
        )
    };

    harness
        .apply_triggers("acme", &file("    allows: [codex, claude]\n"))
        .await;
    let allowed: Vec<String> = harness
        .show_trigger("acme", "ready")
        .await
        .allows
        .into_iter()
        .map(|agent| agent.name)
        .collect();
    assert_eq!(allowed, ["claude", "codex"]);

    let unchanged = harness
        .apply_triggers("acme", &file("    allows: [claude, codex]\n"))
        .await;
    assert!(unchanged.changes.is_empty(), "{:?}", unchanged.changes);

    let narrowed = harness.apply_triggers("acme", &file("")).await;
    assert_eq!(narrowed.changes[0].differences[0].field, "allows");
    assert!(
        harness
            .show_trigger("acme", "ready")
            .await
            .allows
            .is_empty()
    );

    harness.teardown().await;
}
