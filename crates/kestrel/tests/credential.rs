//! Provider Credentials: held by an Organization, encrypted with the key beside the database,
//! and reaching the Harness's own process for the length of one Run and no longer.

mod support;

use std::time::Duration;

use kestrel::domain::{Exit, Run, RunId, Workspace};
use kestrel_scripted_agent::{OTHER_MODEL, Script};
use reqwest::StatusCode;
use support::environment::Environment;
use support::link_client::Link;
use support::supervisor::{self, Supervisor};
use support::{A_PROVIDER_KEY, Kestrel, PROVIDER_KEY, repository, scripted_agent};

const LONG_ENOUGH_TO_BE_SURE: Duration = Duration::from_millis(500);

async fn confiding() -> Kestrel {
    Kestrel::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Confides),
    )
    .await
}

/// A Workspace ready to run in an Organization that holds one Provider Credential, or none.
async fn a_workspace(kestrel: &Kestrel, organization: &str, held: Option<&str>) -> Workspace {
    let declared = kestrel.declare_organization(organization).await;
    kestrel
        .declare_project(
            &declared,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;
    kestrel
        .declare_agent(&declared, "builder", "opencode", Some(OTHER_MODEL))
        .await;
    if let Some(secret) = held {
        kestrel
            .hold_provider_credential(&declared, PROVIDER_KEY, secret)
            .await;
    }

    kestrel
        .open_workspace(organization, repository::NAME, "builder")
        .await
}

/// Answering a turn never ends a Run, so one that answered is stopped, the way a person would.
async fn ended(kestrel: &Kestrel, run: RunId) -> Run {
    kestrel.after_one_turn(run).await
}

async fn transcript(kestrel: &Kestrel, workspace: &Workspace) -> String {
    kestrel
        .transcript(workspace.id)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The agent playing `Confides` says what its own process was spawned with, which is the only
/// place a credential is observable from outside kestrel.
#[tokio::test]
async fn a_run_carries_the_credential_its_organization_holds_into_the_harness() {
    let kestrel = confiding().await;
    let workspace = a_workspace(&kestrel, "acme", Some(A_PROVIDER_KEY)).await;

    let run = kestrel.enqueue_run(workspace.id).await;
    let ended = ended(&kestrel, run.id).await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert!(
        transcript(&kestrel, &workspace)
            .await
            .contains(&format!("{PROVIDER_KEY}={A_PROVIDER_KEY}")),
        "the credential never reached the agent: {}",
        transcript(&kestrel, &workspace).await
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn one_organizations_credential_does_not_reach_anothers_run() {
    let kestrel = confiding().await;
    a_workspace(&kestrel, "acme", Some("the-acme-key")).await;
    let globex = a_workspace(&kestrel, "globex", Some("the-globex-key")).await;

    let run = kestrel.enqueue_run(globex.id).await;
    ended(&kestrel, run.id).await;

    let said = transcript(&kestrel, &globex).await;
    assert!(
        said.contains("the-globex-key"),
        "the organization's own credential never reached its run: {said}"
    );
    assert!(
        !said.contains("the-acme-key"),
        "another organization's credential reached this run: {said}"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_run_whose_organization_holds_no_credential_fails_before_an_instance() {
    let kestrel = confiding().await;
    let workspace = a_workspace(&kestrel, "acme", None).await;

    let run = kestrel.enqueue_run(workspace.id).await;
    let ended = ended(&kestrel, run.id).await;

    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and its organization holds no provider credential",
            ended.exit
        );
    };
    assert!(
        because.contains("holds no provider credential"),
        "unhelpful exit status: {because}"
    );
    assert!(
        ended.instance.is_none(),
        "an instance was provisioned to find out what the control plane already knew"
    );

    kestrel.teardown().await;
}

/// The supervisor starts with what it needs to reach the link, and nothing a provider would
/// accept: what carries the credential is the spawn inside it, one step later.
#[cfg(unix)]
#[tokio::test]
async fn nothing_a_supervisor_is_started_with_carries_a_credential() {
    let environment = Environment::executing(
        "env > \"$(dirname \"$0\")/variables\"\n\
         exit 3",
    );
    let kestrel = Kestrel::dispatching(environment.path()).await;
    let workspace = a_workspace(&kestrel, "acme", Some(A_PROVIDER_KEY)).await;

    let run = kestrel.enqueue_run(workspace.id).await;
    ended(&kestrel, run.id).await;

    let provisioned = environment.wrote("variables");
    assert!(
        provisioned.contains("KESTREL_RUN="),
        "the supervisor wrote down no variables to look through:\n{provisioned}"
    );
    assert!(
        !provisioned.contains(A_PROVIDER_KEY) && !provisioned.contains(PROVIDER_KEY),
        "the supervisor was started with a provider credential:\n{provisioned}"
    );

    kestrel.teardown().await;
}

/// An Environment asks for a credential as it spawns its agent, so one that is never told to
/// start never has one to hold, and nothing is decrypted for it.
#[tokio::test]
async fn an_environment_that_is_never_told_to_start_takes_no_credential() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel, "acme", Some(A_PROVIDER_KEY)).await;
    let (run, credential) = kestrel.dispatch_run(workspace.id).await;

    let mut supervisor = Supervisor::provision(&kestrel.link(), run.id, &credential);
    supervisor.wait_until_it_says("reported connected").await;
    tokio::time::sleep(LONG_ENOUGH_TO_BE_SURE).await;

    assert!(
        !supervisor.said("carrying"),
        "an idle environment took a credential:\n{}",
        supervisor.everything_it_said()
    );

    supervisor.destroy();
    kestrel.teardown().await;
}

#[tokio::test]
async fn the_credentials_a_run_needs_reach_nobody_but_that_run() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel, "acme", Some(A_PROVIDER_KEY)).await;
    let (run, credential) = kestrel.dispatch_run(workspace.id).await;
    let (elsewhere, _) = kestrel
        .dispatch_run(a_workspace(&kestrel, "globex", None).await.id)
        .await;
    let link = Link::to(&kestrel.link());

    assert_eq!(
        link.credentials(run.id, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        link.credentials(elsewhere.id, Some(&credential))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    kestrel.teardown().await;
}

/// A credential is invalidated when its Run ends, so the Workspace's next Run finds nothing on the
/// Instance that could ask for the provider keys again.
#[tokio::test]
async fn a_run_that_has_ended_hands_out_no_credential() {
    let kestrel = Kestrel::boot().await;
    let workspace = a_workspace(&kestrel, "acme", Some(A_PROVIDER_KEY)).await;
    let (run, credential) = kestrel.dispatch_run(workspace.id).await;
    let link = Link::to(&kestrel.link());
    assert_eq!(
        link.credentials(run.id, Some(&credential)).await.status(),
        StatusCode::OK
    );

    kestrel.complete_run(&run).await;

    assert_eq!(
        link.credentials(run.id, Some(&credential)).await.status(),
        StatusCode::UNAUTHORIZED
    );

    kestrel.teardown().await;
}

/// The key is generated the first time kestrel opens a data directory: an operator supplies
/// provider keys, and never a key of kestrel's.
#[tokio::test]
async fn the_key_is_generated_beside_the_database_and_what_it_sealed_is_not_readable_without_it() {
    let kestrel = Kestrel::boot().await;
    let organization = kestrel.declare_organization("acme").await;

    kestrel
        .hold_provider_credential(&organization, PROVIDER_KEY, A_PROVIDER_KEY)
        .await;

    assert!(
        kestrel.data_dir().join("kestrel.key").exists(),
        "no key was generated beside the database"
    );
    for kept in ["kestrel.db", "kestrel.db-wal"] {
        let Ok(written) = std::fs::read(kestrel.data_dir().join(kept)) else {
            continue;
        };
        assert!(
            !contains(&written, A_PROVIDER_KEY),
            "a copy of {kept} is a usable provider credential"
        );
    }

    kestrel.teardown().await;
}

#[tokio::test]
async fn what_an_organization_holds_lists_by_the_variable_it_is_read_from_and_never_by_value() {
    let kestrel = Kestrel::boot().await;
    let organization = kestrel.declare_organization("acme").await;

    kestrel
        .hold_provider_credential(&organization, PROVIDER_KEY, A_PROVIDER_KEY)
        .await;
    let held = kestrel.provider_credentials_held(&organization).await;

    let [only] = &held[..] else {
        panic!("the organization holds {held:?}");
    };
    assert_eq!(only.variable, PROVIDER_KEY);

    kestrel.teardown().await;
}

/// What an agent said reaches the Transcript, and what it was spawned with does not.
#[tokio::test]
async fn a_run_that_used_a_credential_records_it_nowhere() {
    let kestrel = Kestrel::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Speaks),
    )
    .await;
    let workspace = a_workspace(&kestrel, "acme", Some(A_PROVIDER_KEY)).await;

    let run = kestrel.enqueue_run(workspace.id).await;
    let ended = ended(&kestrel, run.id).await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert!(
        !transcript(&kestrel, &workspace)
            .await
            .contains(A_PROVIDER_KEY)
    );
    assert!(!format!("{ended:?}").contains(A_PROVIDER_KEY));

    kestrel.teardown().await;
}

fn contains(written: &[u8], secret: &str) -> bool {
    written
        .windows(secret.len())
        .any(|window| window == secret.as_bytes())
}
