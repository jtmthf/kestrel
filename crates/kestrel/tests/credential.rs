//! Provider Credentials: held by an Organization, encrypted with the key beside the database,
//! and reaching the Agent Runtime's own process for the length of one Run and no longer.

mod support;

use std::time::Duration;

use kestrel::domain::{Exit, Run, RunId, RunState, Session};
use kestrel_scripted_agent::{OTHER_MODEL, Script};
use reqwest::StatusCode;
use support::environment::Environment;
use support::link_client::Link;
use support::supervisor::{self, Supervisor};
use support::{A_PROVIDER_KEY, Harness, PROVIDER_KEY, repository, scripted_agent};

const PATIENCE: Duration = Duration::from_secs(30);
const LONG_ENOUGH_TO_BE_SURE: Duration = Duration::from_millis(500);

async fn confiding() -> Harness {
    Harness::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Confides),
    )
    .await
}

/// A Session ready to run in an Organization that holds one Provider Credential, or none.
async fn a_session(harness: &Harness, organization: &str, held: Option<&str>) -> Session {
    let declared = harness.declare_organization(organization).await;
    harness
        .declare_workspace(
            &declared,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;
    harness
        .declare_agent(&declared, "builder", "opencode", OTHER_MODEL)
        .await;
    if let Some(secret) = held {
        harness
            .hold_provider_credential(&declared, PROVIDER_KEY, secret)
            .await;
    }

    harness
        .open_session(organization, repository::NAME, "builder")
        .await
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

async fn transcript(harness: &Harness, session: &Session) -> String {
    harness
        .transcript(session.id)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The agent playing `Confides` says what its own process was spawned with, which is the only
/// place a credential is observable from outside kestrel.
#[tokio::test]
async fn a_run_carries_the_credential_its_organization_holds_into_the_agent_runtime() {
    let harness = confiding().await;
    let session = a_session(&harness, "acme", Some(A_PROVIDER_KEY)).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert!(
        transcript(&harness, &session)
            .await
            .contains(&format!("{PROVIDER_KEY}={A_PROVIDER_KEY}")),
        "the credential never reached the agent: {}",
        transcript(&harness, &session).await
    );

    harness.teardown().await;
}

#[tokio::test]
async fn one_organizations_credential_does_not_reach_anothers_run() {
    let harness = confiding().await;
    a_session(&harness, "acme", Some("the-acme-key")).await;
    let globex = a_session(&harness, "globex", Some("the-globex-key")).await;

    let run = harness.enqueue_run(globex.id).await;
    ended(&harness, run.id).await;

    let said = transcript(&harness, &globex).await;
    assert!(
        said.contains("the-globex-key"),
        "the organization's own credential never reached its run: {said}"
    );
    assert!(
        !said.contains("the-acme-key"),
        "another organization's credential reached this run: {said}"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_whose_organization_holds_no_credential_fails_before_an_environment() {
    let harness = confiding().await;
    let session = a_session(&harness, "acme", None).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

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
        ended.environment.is_none(),
        "an environment was provisioned to find out what the control plane already knew"
    );

    harness.teardown().await;
}

/// The Environment is provisioned with what it needs to reach the link, and nothing a provider
/// would accept: what carries the credential is the spawn inside it, one Run later.
#[cfg(unix)]
#[tokio::test]
async fn nothing_an_environment_is_provisioned_with_carries_a_credential() {
    let environment = Environment::executing(
        "env > \"$(dirname \"$0\")/variables\"\n\
         exit 3",
    );
    let harness = Harness::dispatching(environment.path()).await;
    let session = a_session(&harness, "acme", Some(A_PROVIDER_KEY)).await;

    let run = harness.enqueue_run(session.id).await;
    ended(&harness, run.id).await;

    let provisioned = environment.wrote("variables");
    assert!(
        provisioned.contains("KESTREL_RUN="),
        "the environment wrote down no variables to look through:\n{provisioned}"
    );
    assert!(
        !provisioned.contains(A_PROVIDER_KEY) && !provisioned.contains(PROVIDER_KEY),
        "the environment was provisioned with a provider credential:\n{provisioned}"
    );

    harness.teardown().await;
}

/// An Environment asks for a credential as it spawns its agent, so one that is never told to
/// start never has one to hold, and nothing is decrypted for it.
#[tokio::test]
async fn an_environment_that_is_never_told_to_start_takes_no_credential() {
    let harness = Harness::boot().await;
    let session = a_session(&harness, "acme", Some(A_PROVIDER_KEY)).await;
    let (run, credential) = harness.dispatch_run(session.id).await;

    let mut supervisor = Supervisor::provision(&harness.link(), run.id, &credential);
    supervisor.wait_until_it_says("reported connected").await;
    tokio::time::sleep(LONG_ENOUGH_TO_BE_SURE).await;

    assert!(
        !supervisor.said("carrying"),
        "an idle environment took a credential:\n{}",
        supervisor.everything_it_said()
    );

    supervisor.destroy();
    harness.teardown().await;
}

#[tokio::test]
async fn the_credentials_a_run_needs_reach_nobody_but_that_run() {
    let harness = Harness::boot().await;
    let session = a_session(&harness, "acme", Some(A_PROVIDER_KEY)).await;
    let (run, credential) = harness.dispatch_run(session.id).await;
    let (elsewhere, _) = harness
        .dispatch_run(a_session(&harness, "globex", None).await.id)
        .await;
    let link = Link::to(&harness.link());

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

    harness.teardown().await;
}

/// A credential is invalidated when its Run ends, so a destroyed Environment holding one could
/// not ask for the provider keys again even if it were still there to ask.
#[tokio::test]
async fn a_run_that_has_ended_hands_out_no_credential() {
    let harness = Harness::boot().await;
    let session = a_session(&harness, "acme", Some(A_PROVIDER_KEY)).await;
    let (run, credential) = harness.dispatch_run(session.id).await;
    let link = Link::to(&harness.link());
    assert_eq!(
        link.credentials(run.id, Some(&credential)).await.status(),
        StatusCode::OK
    );

    harness.complete_run(&run).await;

    assert_eq!(
        link.credentials(run.id, Some(&credential)).await.status(),
        StatusCode::UNAUTHORIZED
    );

    harness.teardown().await;
}

/// The key is generated the first time kestrel opens a data directory: an operator supplies
/// provider keys, and never a key of kestrel's.
#[tokio::test]
async fn the_key_is_generated_beside_the_database_and_what_it_sealed_is_not_readable_without_it() {
    let harness = Harness::boot().await;
    let organization = harness.declare_organization("acme").await;

    harness
        .hold_provider_credential(&organization, PROVIDER_KEY, A_PROVIDER_KEY)
        .await;

    assert!(
        harness.data_dir().join("kestrel.key").exists(),
        "no key was generated beside the database"
    );
    for kept in ["kestrel.db", "kestrel.db-wal"] {
        let Ok(written) = std::fs::read(harness.data_dir().join(kept)) else {
            continue;
        };
        assert!(
            !contains(&written, A_PROVIDER_KEY),
            "a copy of {kept} is a usable provider credential"
        );
    }

    harness.teardown().await;
}

#[tokio::test]
async fn what_an_organization_holds_lists_by_the_variable_it_is_read_from_and_never_by_value() {
    let harness = Harness::boot().await;
    let organization = harness.declare_organization("acme").await;

    harness
        .hold_provider_credential(&organization, PROVIDER_KEY, A_PROVIDER_KEY)
        .await;
    let held = harness.provider_credentials_held(&organization).await;

    let [only] = &held[..] else {
        panic!("the organization holds {held:?}");
    };
    assert_eq!(only.variable, PROVIDER_KEY);

    harness.teardown().await;
}

/// What an agent said reaches the Transcript, and what it was spawned with does not.
#[tokio::test]
async fn a_run_that_used_a_credential_records_it_nowhere() {
    let harness = Harness::dispatching_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Speaks),
    )
    .await;
    let session = a_session(&harness, "acme", Some(A_PROVIDER_KEY)).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert!(
        !transcript(&harness, &session)
            .await
            .contains(A_PROVIDER_KEY)
    );
    assert!(!format!("{ended:?}").contains(A_PROVIDER_KEY));

    harness.teardown().await;
}

fn contains(written: &[u8], secret: &str) -> bool {
    written
        .windows(secret.len())
        .any(|window| window == secret.as_bytes())
}
