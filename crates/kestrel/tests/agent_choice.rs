//! A Session starts with its Trigger's Agent, or the one an `agent:<name>` label chooses from
//! those the Trigger allows, and keeps that Agent's harness and model for as long as it is open.

mod support;

use std::time::Duration;

use jiff::SignedDuration;
use kestrel::domain::{Direction, Exit, Run, RunId, Session};
use kestrel_scripted_agent::{DEFAULT_MODEL, OTHER_MODEL};
use serde_json::Value;
use support::github_stub::{self, GithubStub};
use support::scripted_agent::{self, Script};
use support::{Kestrel, client, repository, supervisor};

const PATIENCE: Duration = Duration::from_secs(30);
const REPOSITORY: &str = "jtmthf/kestrel";
const READY: &str = "ready-for-agent";
const BOTH: &[Direction] = &[Direction::Inbound, Direction::Outbound];

/// `builder` on the default harness, and `codex` and `claude` on harnesses of their own, each
/// naming a model so a test can tell whose was recorded.
async fn an_organization(kestrel: &Kestrel) {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(
            &organization,
            "kestrel",
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;
    kestrel
        .declare_agent(
            &organization,
            "builder",
            support::HARNESS,
            Some(DEFAULT_MODEL),
        )
        .await;
    kestrel
        .declare_agent(&organization, "codex", "codex", Some(OTHER_MODEL))
        .await;
    kestrel
        .declare_agent(&organization, "claude", "claude", None)
        .await;
    kestrel
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;
}

async fn watching(kestrel: &Kestrel, stub: &GithubStub) {
    kestrel
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
async fn labelled(kestrel: &Kestrel, labels: &[&str]) -> GithubStub {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled_carrying(
        7, 43, READY, labels,
    )]));
    kestrel
        .declare_trigger_allowing("acme", REPOSITORY, "builder", &["codex"], None)
        .await;
    watching(kestrel, &stub).await;
    stub
}

async fn opened(kestrel: &Kestrel) -> Session {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        if let Some(session) = kestrel.sessions("acme").await.into_iter().next() {
            return kestrel.show_session(session.id).await;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no session was ever opened"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The reason the Trigger's firing gave for starting nothing, as a Client is shown it.
async fn refused(kestrel: &Kestrel) -> String {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        if let Some(event) = kestrel.events("acme").await.first() {
            let record = event.record_id.to_string();
            let operator = kestrel.operator();
            let shown = tokio::task::spawn_blocking(move || {
                client::ran(&operator, &["event", "show", &record, "--json", "firings"])
            })
            .await
            .expect("the client should run");
            assert!(shown.status.success(), "{}", shown.err);
            let shown: Value = serde_json::from_str(&shown.out[0]).expect("a record");

            if let Some(firing) = shown["firings"].as_array().and_then(|all| all.first()) {
                assert_eq!(firing["outcome"], "failed", "{firing}");
                assert!(
                    kestrel.sessions("acme").await.is_empty(),
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

/// Answering a turn never ends a Run, so one that answered is stopped, the way a person would.
async fn ended(kestrel: &Kestrel, run: RunId) -> Run {
    kestrel.after_one_turn(run).await
}

#[tokio::test]
async fn with_no_agent_label_a_session_starts_with_the_triggers_agent() {
    let kestrel = Kestrel::boot().await;
    an_organization(&kestrel).await;
    let _github = labelled(&kestrel, &["bug"]).await;

    let session = opened(&kestrel).await;

    assert_eq!(session.agent.name, "builder");
    assert_eq!(session.agent.harness, support::HARNESS);
    assert_eq!(session.agent.model.as_deref(), Some(DEFAULT_MODEL));

    kestrel.teardown().await;
}

#[tokio::test]
async fn one_agent_label_chooses_an_agent_the_trigger_allows() {
    let kestrel = Kestrel::boot().await;
    an_organization(&kestrel).await;
    let _github = labelled(&kestrel, &["agent:codex"]).await;

    let session = opened(&kestrel).await;

    assert_eq!(session.agent.name, "codex");
    assert_eq!(session.agent.harness, "codex");
    assert_eq!(session.agent.model.as_deref(), Some(OTHER_MODEL));

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_label_choosing_an_agent_the_trigger_does_not_allow_starts_nothing_and_says_why() {
    let kestrel = Kestrel::boot().await;
    an_organization(&kestrel).await;
    let _github = labelled(&kestrel, &["agent:claude"]).await;

    assert_eq!(
        refused(&kestrel).await,
        "the label agent:claude chooses an agent the trigger ready does not allow"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn labels_choosing_two_agents_start_nothing_and_say_why() {
    let kestrel = Kestrel::boot().await;
    an_organization(&kestrel).await;
    let _github = labelled(&kestrel, &["agent:codex", "agent:builder"]).await;

    assert_eq!(
        refused(&kestrel).await,
        "the labels agent:builder and agent:codex each choose an agent, and the trigger ready \
         will not guess which"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_label_on_work_that_feeds_an_open_session_changes_nothing_about_its_agent() {
    let kestrel = Kestrel::boot().await;
    an_organization(&kestrel).await;
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[
        github_stub::labelled_carrying(7, 43, READY, &["agent:codex"]),
        github_stub::labelled_carrying(8, 43, READY, &["agent:codex", "agent:builder"]),
    ]));
    kestrel
        .declare_trigger_allowing(
            "acme",
            REPOSITORY,
            "builder",
            &["codex"],
            Some("{{ event.source }}{{ event.subject }}"),
        )
        .await;
    watching(&kestrel, &stub).await;
    let session = opened(&kestrel).await;

    let deadline = tokio::time::Instant::now() + PATIENCE;
    while kestrel.transcript(session.id).await.len() < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the second event never fed the session"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(kestrel.sessions("acme").await.len(), 1);
    assert_eq!(kestrel.show_session(session.id).await.agent.name, "codex");

    kestrel.teardown().await;
}

#[tokio::test]
async fn an_open_sessions_agent_keeps_the_harness_and_model_it_opened_with() {
    let kestrel = Kestrel::boot().await;
    an_organization(&kestrel).await;
    let session = kestrel.open_session("acme", "kestrel", "codex").await;
    let organization = &session.organization;

    kestrel
        .declare_agent(organization, "codex", support::HARNESS, None)
        .await;
    kestrel
        .set_agent_model(organization, "codex", Some(DEFAULT_MODEL))
        .await;

    let shown = kestrel.show_session(session.id).await;
    assert_eq!(shown.agent.harness, "codex");
    assert_eq!(shown.agent.model.as_deref(), Some(OTHER_MODEL));
    let later = kestrel.open_session("acme", "kestrel", "codex").await;
    assert_eq!(later.agent.harness, support::HARNESS);
    assert_eq!(later.agent.model.as_deref(), Some(DEFAULT_MODEL));

    kestrel.teardown().await;
}

/// The default harness dies, so only the harness the label chose can end the Run well, and only
/// on the model the chosen Agent named.
#[tokio::test]
async fn the_work_role_runs_the_harness_and_model_a_label_chose() {
    let kestrel = Kestrel::dispatching_harnesses(
        supervisor::binary(),
        &[
            (support::HARNESS, &scripted_agent::playing(Script::Dies)),
            ("codex", &scripted_agent::playing(Script::Speaks)),
        ],
    )
    .await;
    an_organization(&kestrel).await;
    let _github = labelled(&kestrel, &["agent:codex"]).await;
    let session = opened(&kestrel).await;
    let organization = &session.organization;
    kestrel
        .set_agent_model(organization, "codex", Some(DEFAULT_MODEL))
        .await;

    let run = kestrel.runs(session.id).await.remove(0);
    let run = ended(&kestrel, run.id).await;

    assert_eq!(run.exit, Some(Exit::Succeeded), "{:?}", run.exit);
    assert_eq!(run.worked_model.as_deref(), Some(OTHER_MODEL));

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_harness_the_work_role_cannot_spawn_fails_the_run_and_says_which() {
    let kestrel = Kestrel::dispatching(supervisor::binary()).await;
    an_organization(&kestrel).await;
    let session = kestrel.open_session("acme", "kestrel", "claude").await;
    let run = kestrel.enqueue_run(session.id).await;

    let run = ended(&kestrel, run.id).await;

    assert_eq!(
        run.exit,
        Some(Exit::Failed {
            because: "this work role spawns no harness named claude".to_owned()
        })
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_declaration_file_names_the_agents_a_label_may_choose() {
    let kestrel = Kestrel::boot().await;
    an_organization(&kestrel).await;
    let file = |allows: &str| {
        format!(
            "triggers:\n  ready:\n    filter: {}\n    brief: Work\n    project: kestrel\n    \
             agent: builder\n{allows}",
            support::labelled_on(REPOSITORY, READY)
        )
    };

    kestrel
        .apply_triggers("acme", &file("    allows: [codex, claude]\n"))
        .await;
    let allowed: Vec<String> = kestrel
        .show_trigger("acme", "ready")
        .await
        .allows
        .into_iter()
        .map(|agent| agent.name)
        .collect();
    assert_eq!(allowed, ["claude", "codex"]);

    let unchanged = kestrel
        .apply_triggers("acme", &file("    allows: [claude, codex]\n"))
        .await;
    assert!(unchanged.changes.is_empty(), "{:?}", unchanged.changes);

    let narrowed = kestrel.apply_triggers("acme", &file("")).await;
    assert_eq!(narrowed.changes[0].differences[0].field, "allows");
    assert!(
        kestrel
            .show_trigger("acme", "ready")
            .await
            .allows
            .is_empty()
    );

    kestrel.teardown().await;
}
