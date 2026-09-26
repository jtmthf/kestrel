mod support;

use kestrel::domain::{Exit, RunId, WorkspaceId};
use kestrel::log::Entry;
use serde_json::Value;
use support::client::{Finished, Invocation, Shown, ran_by, ran_on_a_terminal_by};
use support::scripted_agent::{self, Script};
use support::{A_PROVIDER_KEY, Kestrel, PROVIDER_KEY, repository, supervisor};

const BRIEF: &str = "Make the README say what kestrel is";
const STARTED: &str = "organization,project,agent,workspace,workspace_id,run,run_id";
const QUESTION: &str = "apply this plan?";

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

/// Each explained value without its reason, which names the directory the clone was made in.
fn resolved(said: &str) -> Vec<String> {
    said.lines()
        .skip_while(|line| *line != "starting work with")
        .skip(1)
        .take(8)
        .map(|line| line.split("  (").next().unwrap_or(line).to_owned())
        .collect()
}

fn reached(shown: &Shown) -> Value {
    let record = shown
        .lines()
        .into_iter()
        .find(|line| line.starts_with('{'))
        .unwrap_or_else(|| panic!("no record reached the terminal:\n{}", shown.said));

    serde_json::from_str(record).expect("a record")
}

async fn declared(kestrel: &Kestrel) -> Vec<String> {
    let mut declared = Vec::new();
    for organization in kestrel.organizations().await {
        declared.push(format!("organization {}", organization.name));
        for project in kestrel.projects(&organization).await {
            declared.push(format!(
                "project {} {:?} {}",
                project.name, project.repositories, project.branch
            ));
        }
        for agent in kestrel.agents(&organization).await {
            declared.push(format!(
                "agent {} {} {:?}",
                agent.name, agent.harness, agent.model
            ));
        }
        for held in kestrel.provider_credentials_held(&organization).await {
            declared.push(format!("credential {}", held.variable));
        }
        declared.push(format!(
            "workspaces {}",
            kestrel.workspaces(&organization.name).await.len()
        ));
    }

    declared
}

#[tokio::test]
async fn one_command_takes_a_fresh_clone_and_an_empty_control_plane_to_a_run_carrying_its_brief() {
    let kestrel = Kestrel::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Echoes),
    )
    .await;
    assert!(kestrel.organizations().await.is_empty());

    let started = ran_by(
        &kestrel,
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
    assert_eq!(started["project"], repository::NAME);
    assert_eq!(started["agent"], "opencode");
    let workspace: WorkspaceId = started["workspace_id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .expect("a workspace identifier");
    let run: RunId = started["run_id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .expect("a run identifier");

    let ended = kestrel.after_one_turn(run).await;
    assert_eq!(ended.exit, Some(Exit::Succeeded));
    let transcript = kestrel.transcript(workspace).await;
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

    kestrel.teardown().await;
}

#[tokio::test]
async fn every_inferred_value_is_explained_before_anything_is_applied() {
    let kestrel = Kestrel::boot().await;

    let started = ran_by(&kestrel, &["start", "--brief", BRIEF], in_a_fresh_clone()).await;

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
        ("project", "--project"),
        ("repository", "--repository"),
        ("branch", "--branch"),
        ("agent", "--agent"),
        ("harness", "--harness"),
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

    kestrel.teardown().await;
}

#[tokio::test]
async fn flags_say_every_value_nothing_needs_inferring() {
    let kestrel = Kestrel::boot().await;

    let started = ran_by(
        &kestrel,
        &[
            "start",
            "--brief",
            BRIEF,
            "--organization",
            "acme",
            "--project",
            "widgets",
            "--repository",
            repository::url(),
            "--branch",
            repository::EXISTING_BRANCH,
            "--agent",
            "builder",
            "--harness",
            "opencode",
            "--json",
            STARTED,
        ],
        Invocation::default(),
    )
    .await;

    let started = started.records().remove(0);
    assert_eq!(started["organization"], "acme");
    assert_eq!(started["project"], "widgets");
    assert_eq!(started["agent"], "builder");
    let acme = &kestrel.organizations().await[0];
    assert_eq!(
        kestrel.projects(acme).await[0].branch,
        repository::EXISTING_BRANCH
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn with_no_terminal_and_no_clone_it_fails_naming_the_flags_and_declares_nothing() {
    let kestrel = Kestrel::boot().await;

    let refused = ran_by(
        &kestrel,
        &["start", "--brief", BRIEF, "--credential", PROVIDER_KEY],
        Invocation::default().given(""),
    )
    .await;

    refused_naming(
        &refused,
        &["--repository", "--branch", "--credential", PROVIDER_KEY],
    );
    assert!(kestrel.organizations().await.is_empty());

    kestrel.teardown().await;
}

#[tokio::test]
async fn with_several_organizations_and_none_named_it_fails_naming_the_flag() {
    let kestrel = Kestrel::boot().await;
    kestrel.declare_organization("acme").await;
    kestrel.declare_organization("globex").await;

    let refused = ran_by(&kestrel, &["start", "--brief", BRIEF], in_a_fresh_clone()).await;

    refused_naming(&refused, &["--organization", "acme", "globex"]);
    for organization in kestrel.organizations().await {
        assert!(kestrel.projects(&organization).await.is_empty());
    }

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_plan_the_control_plane_refuses_leaves_no_partial_setup() {
    let kestrel = Kestrel::boot().await;
    let acme = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(
            &acme,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;

    let refused = ran_by(
        &kestrel,
        &[
            "start",
            "--brief",
            BRIEF,
            "--project",
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
        kestrel.projects(&acme).await[0].repositories,
        [repository::url()]
    );
    assert!(kestrel.agents(&acme).await.is_empty());
    assert!(kestrel.provider_credentials_held(&acme).await.is_empty());
    assert!(kestrel.workspaces("acme").await.is_empty());

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_declaration_the_plan_would_change_is_named_before_anything_is_sent() {
    let kestrel = Kestrel::boot().await;
    let acme = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(
            &acme,
            repository::NAME,
            &[repository::other_url().to_owned()],
            repository::BRANCH,
        )
        .await;

    let refused = ran_by(&kestrel, &["start", "--brief", BRIEF], in_a_fresh_clone()).await;

    refused_naming(&refused, &["--project"]);
    assert!(kestrel.workspaces("acme").await.is_empty());

    kestrel.teardown().await;
}

#[tokio::test]
async fn on_a_terminal_confirming_once_applies_the_plan_the_noninteractive_start_applies() {
    let args = [
        "start",
        "--brief",
        BRIEF,
        "--credential",
        PROVIDER_KEY,
        "--json",
        STARTED,
    ];
    let invocation = in_a_fresh_clone().env(PROVIDER_KEY, A_PROVIDER_KEY);
    let (interactive, noninteractive) = (Kestrel::boot().await, Kestrel::boot().await);

    let confirmed = ran_on_a_terminal_by(&interactive, &args, invocation.clone(), "y\n").await;
    let applied = ran_by(&noninteractive, &args, invocation).await;

    assert!(confirmed.status.success(), "{}", confirmed.said);
    assert_eq!(
        confirmed.said.matches(QUESTION).count(),
        1,
        "the start asked something other than one confirmation:\n{}",
        confirmed.said
    );
    assert!(
        !applied.err.contains(QUESTION),
        "a start with no terminal asked:\n{}",
        applied.err
    );
    assert_eq!(resolved(&confirmed.said), resolved(&applied.err));
    let (confirmed, applied) = (reached(&confirmed), applied.records().remove(0));
    for field in ["organization", "project", "agent"] {
        assert_eq!(confirmed[field], applied[field], "{field}");
    }
    assert_eq!(
        declared(&interactive).await,
        declared(&noninteractive).await
    );

    interactive.teardown().await;
    noninteractive.teardown().await;
}

#[tokio::test]
async fn on_a_terminal_the_plan_is_explained_and_taught_and_declining_it_changes_nothing() {
    let kestrel = Kestrel::boot().await;

    let shown = ran_on_a_terminal_by(
        &kestrel,
        &["start", "--brief", BRIEF],
        in_a_fresh_clone(),
        "n\n",
    )
    .await;

    assert!(shown.status.success(), "{}", shown.said);
    assert!(kestrel.organizations().await.is_empty());
    let before: Vec<&str> = shown
        .lines()
        .into_iter()
        .take_while(|line| !line.contains(QUESTION))
        .collect();
    assert_eq!(resolved(&before.join("\n")).len(), 8, "{}", shown.said);
    for flag in [
        "--organization",
        "--project",
        "--repository",
        "--branch",
        "--agent",
        "--harness",
        "--model",
        "--credential",
    ] {
        assert!(
            before
                .iter()
                .any(|line| line.ends_with(&format!("{flag} overrides it)"))),
            "no value names {flag}:\n{}",
            shown.said
        );
    }
    for taught in [
        "declare the Organization default",
        &format!("declare the Project {}", repository::NAME),
        "declare the Agent opencode",
        "open a Workspace",
    ] {
        assert!(
            before.iter().any(|line| line.contains(taught)),
            "the plan does not say it will {taught}:\n{}",
            shown.said
        );
    }

    kestrel.teardown().await;
}

#[tokio::test]
async fn on_a_terminal_yes_applies_the_plan_without_asking() {
    let kestrel = Kestrel::boot().await;

    let applied = ran_on_a_terminal_by(
        &kestrel,
        &["start", "--brief", BRIEF, "--yes"],
        in_a_fresh_clone(),
        "",
    )
    .await;

    assert!(applied.status.success(), "{}", applied.said);
    assert!(!applied.said.contains(QUESTION), "{}", applied.said);
    assert_eq!(kestrel.workspaces("default").await.len(), 1);

    kestrel.teardown().await;
}

#[tokio::test]
async fn with_output_on_a_terminal_and_input_piped_nothing_is_asked() {
    let kestrel = Kestrel::boot().await;

    let applied = ran_on_a_terminal_by(
        &kestrel,
        &["start", "--brief", BRIEF],
        in_a_fresh_clone().given(""),
        "",
    )
    .await;

    assert!(applied.status.success(), "{}", applied.said);
    assert!(!applied.said.contains(QUESTION), "{}", applied.said);
    assert_eq!(kestrel.workspaces("default").await.len(), 1);

    kestrel.teardown().await;
}
