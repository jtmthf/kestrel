mod support;

use kestrel::domain::{Exit, RunId, SessionId};
use kestrel::log::Entry;
use support::client::{Finished, Invocation, ran_by};
use support::scripted_agent::{self, Script};
use support::{A_PROVIDER_KEY, Harness, PROVIDER_KEY, repository, supervisor};

const BRIEF: &str = "Make the README say what kestrel is";
const STARTED: &str = "organization,workspace,agent,session,session_id,run,run_id";

fn in_a_fresh_clone() -> Invocation {
    Invocation::default()
        .cloned(repository::url(), repository::NAME)
        .within(repository::NAME)
}

fn refused_naming(finished: &Finished, flags: &[&str]) {
    assert_eq!(
        finished.status.code(),
        Some(2),
        "the start was not refused as a usage error:\n{}",
        finished.err
    );
    assert!(finished.out.is_empty(), "{:?}", finished.out);
    for flag in flags {
        assert!(
            finished.err.contains(flag),
            "the refusal does not name {flag}:\n{}",
            finished.err
        );
    }
}

#[tokio::test]
async fn one_command_takes_a_fresh_clone_and_an_empty_control_plane_to_a_run_carrying_its_brief() {
    let harness = Harness::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Echoes),
    )
    .await;
    assert!(harness.organizations().await.is_empty());

    let started = ran_by(
        &harness,
        &[
            "start",
            "--brief",
            BRIEF,
            "--credential",
            PROVIDER_KEY,
            "--json",
            STARTED,
        ],
        in_a_fresh_clone().env(PROVIDER_KEY, A_PROVIDER_KEY),
    )
    .await;

    let started = started.records().remove(0);
    assert_eq!(started["organization"], "default");
    assert_eq!(started["workspace"], repository::NAME);
    assert_eq!(started["agent"], "opencode");
    let session: SessionId = started["session_id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .expect("a session identifier");
    let run: RunId = started["run_id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .expect("a run identifier");

    let ended = harness.after_one_turn(run).await;
    assert_eq!(ended.exit, Some(Exit::Succeeded));
    let transcript = harness.transcript(session).await;
    assert_eq!(
        transcript[0].entry,
        Entry::Brief {
            trigger: None,
            brief: BRIEF.to_owned(),
        }
    );
    assert!(
        transcript.iter().any(|recorded| recorded.entry
            == Entry::Said {
                participant: "opencode".to_owned(),
                message: BRIEF.to_owned(),
            }),
        "the agent was never prompted with the brief"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn every_inferred_value_is_explained_before_anything_is_applied() {
    let harness = Harness::boot().await;

    let started = ran_by(&harness, &["start", "--brief", BRIEF], in_a_fresh_clone()).await;

    assert!(started.status.success(), "{}", started.err);
    let explained: Vec<&str> = started
        .err
        .lines()
        .skip_while(|line| *line != "starting work with")
        .skip(1)
        .take(8)
        .collect();
    for (line, (what, flag)) in explained.iter().zip([
        ("organization", "--organization"),
        ("workspace", "--workspace"),
        ("repository", "--repository"),
        ("branch", "--branch"),
        ("agent", "--agent"),
        ("runtime", "--runtime"),
        ("model", "--model"),
        ("credentials", "--credential"),
    ]) {
        assert!(
            line.trim_start().starts_with(what) && line.ends_with(&format!("{flag} overrides it)")),
            "{what} is not explained with the flag that overrides it:\n{}",
            started.err
        );
    }
    assert!(
        explained[2].contains(repository::url()) && explained[2].contains("origin of the clone"),
        "{}",
        started.err
    );
    assert!(
        explained[3].contains("main  (origin's default branch"),
        "{}",
        started.err
    );

    harness.teardown().await;
}

#[tokio::test]
async fn flags_say_every_value_nothing_needs_inferring() {
    let harness = Harness::boot().await;

    let started = ran_by(
        &harness,
        &[
            "start",
            "--brief",
            BRIEF,
            "--organization",
            "acme",
            "--workspace",
            "widgets",
            "--repository",
            repository::url(),
            "--branch",
            repository::EXISTING_BRANCH,
            "--agent",
            "builder",
            "--runtime",
            "opencode",
            "--json",
            STARTED,
        ],
        Invocation::default(),
    )
    .await;

    let started = started.records().remove(0);
    assert_eq!(started["organization"], "acme");
    assert_eq!(started["workspace"], "widgets");
    assert_eq!(started["agent"], "builder");
    let acme = &harness.organizations().await[0];
    assert_eq!(
        harness.workspaces(acme).await[0].branch,
        repository::EXISTING_BRANCH
    );

    harness.teardown().await;
}

#[tokio::test]
async fn with_no_terminal_and_no_clone_it_fails_naming_the_flags_and_declares_nothing() {
    let harness = Harness::boot().await;

    let refused = ran_by(
        &harness,
        &["start", "--brief", BRIEF, "--credential", PROVIDER_KEY],
        Invocation::default().given(""),
    )
    .await;

    refused_naming(
        &refused,
        &["--repository", "--branch", "--credential", PROVIDER_KEY],
    );
    assert!(harness.organizations().await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn with_several_organizations_and_none_named_it_fails_naming_the_flag() {
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;
    harness.declare_organization("globex").await;

    let refused = ran_by(&harness, &["start", "--brief", BRIEF], in_a_fresh_clone()).await;

    refused_naming(&refused, &["--organization", "acme", "globex"]);
    for organization in harness.organizations().await {
        assert!(harness.workspaces(&organization).await.is_empty());
    }

    harness.teardown().await;
}

#[tokio::test]
async fn a_plan_the_control_plane_refuses_leaves_no_partial_setup() {
    let harness = Harness::boot().await;
    let acme = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &acme,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;

    let refused = ran_by(
        &harness,
        &[
            "start",
            "--brief",
            BRIEF,
            "--workspace",
            repository::NAME,
            "--repository",
            repository::other_url(),
            "--credential",
            PROVIDER_KEY,
        ],
        in_a_fresh_clone().env(PROVIDER_KEY, A_PROVIDER_KEY),
    )
    .await;

    assert_eq!(refused.status.code(), Some(4), "{}", refused.err);
    assert_eq!(
        harness.workspaces(&acme).await[0].repositories,
        [repository::url()]
    );
    assert!(harness.agents(&acme).await.is_empty());
    assert!(harness.provider_credentials_held(&acme).await.is_empty());
    assert!(harness.sessions("acme").await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_declaration_the_plan_would_change_is_named_before_anything_is_sent() {
    let harness = Harness::boot().await;
    let acme = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &acme,
            repository::NAME,
            &[repository::other_url().to_owned()],
            repository::BRANCH,
        )
        .await;

    let refused = ran_by(&harness, &["start", "--brief", BRIEF], in_a_fresh_clone()).await;

    refused_naming(&refused, &["--workspace"]);
    assert!(harness.sessions("acme").await.is_empty());

    harness.teardown().await;
}
