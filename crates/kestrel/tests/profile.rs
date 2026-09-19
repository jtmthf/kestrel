//! Subscription Profiles: a person's login, sealed beside the Provider Credentials, reaching only
//! the Runs of Sessions that name it, and outliving every Instance it is written into.

mod support;

use kestrel::domain::{Exit, Run, RunState, Session};
use kestrel::profile::Entry;
use kestrel_scripted_agent::{LOGIN, REFRESHED, Script};
use reqwest::StatusCode;
use support::link_client::Link;
use support::supervisor;
use support::{A_PROVIDER_KEY, Harness, PROVIDER_KEY, SERIALIZED, repository, scripted_agent};

/// Confided by the scripted agent, because it carries the prefix `Confides` says out loud.
const SUBSCRIPTION_KEY: &str = "SCRIPTED_SUBSCRIPTION_KEY";
const JACKS_KEY: &str = "jacks-subscription-key";
const ALEXS_KEY: &str = "alexs-subscription-key";
const FIRST_LOGIN: &str = "a-first-login";

async fn playing(script: Script) -> Harness {
    Harness::dispatching_to(supervisor::binary(), &scripted_agent::playing(script)).await
}

/// An Organization holding no Provider Credential, so a model is reached through a profile or
/// not at all.
async fn declared(harness: &Harness, runtime: &str) {
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
        .declare_agent(&organization, "builder", runtime, None)
        .await;
}

async fn a_profile(harness: &Harness, name: &str, owner: &str, entry: &Entry, login: &str) {
    harness
        .declare_profile("acme", name, owner)
        .await
        .expect("the profile should declare");
    harness.hold_in_profile("acme", name, entry, login).await;
}

fn subscription_key() -> Entry {
    Entry::variable(SUBSCRIPTION_KEY).expect("a variable")
}

fn login_file() -> Entry {
    Entry::file(LOGIN).expect("a file")
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

/// Only an explicit stop, a sealed Session, or a failure ends a Run (ADR-0024): a Run whose
/// agent answered stays open between turns until this stops it, and one that already failed
/// before an agent ever answered is left as it ended.
async fn worked(harness: &Harness, session: &Session) -> (Run, String) {
    let run = harness.enqueue_run(session.id).await;
    let mut run = harness.answered(run.id, 1).await;
    if run.state != RunState::Ended {
        harness.stop_run(run.id).await;
        run = harness.run(run.id).await;
        // The Run ends in the database the moment it is told to stop; what its supervisor holds
        // of the profile is only gone once the supervisor itself has left.
        support::environment::Environment::named(
            run.supervisor.as_deref().expect("a stopped run had a supervisor"),
        )
        .is_gone()
        .await;
    }

    (run, transcript(harness, session).await)
}

#[tokio::test]
async fn a_run_reaches_a_model_with_its_sessions_profile_and_no_provider_account() {
    let harness = playing(Script::Confides).await;
    declared(&harness, "opencode").await;
    a_profile(&harness, "jack", "Jack", &subscription_key(), JACKS_KEY).await;
    let session = harness
        .open_session_with("acme", repository::NAME, "builder", "jack")
        .await;

    let (run, said) = worked(&harness, &session).await;

    assert_eq!(run.exit, Some(Exit::Succeeded), "{said}");
    assert!(
        said.contains(&format!("{SUBSCRIPTION_KEY}={JACKS_KEY}")),
        "the profile never reached the agent: {said}"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn one_persons_profile_reaches_no_session_that_does_not_name_it() {
    let harness = playing(Script::Confides).await;
    declared(&harness, "opencode").await;
    a_profile(&harness, "jack", "Jack", &subscription_key(), JACKS_KEY).await;
    a_profile(&harness, "alex", "Alex", &subscription_key(), ALEXS_KEY).await;
    let organization = harness.organizations().await.remove(0);
    harness
        .hold_provider_credential(&organization, PROVIDER_KEY, A_PROVIDER_KEY)
        .await;

    let alexs = harness
        .open_session_with("acme", repository::NAME, "builder", "alex")
        .await;
    let nobodys = harness
        .open_session("acme", repository::NAME, "builder")
        .await;
    let (_, alex_said) = worked(&harness, &alexs).await;
    let (_, nobody_said) = worked(&harness, &nobodys).await;

    assert!(alex_said.contains(ALEXS_KEY), "{alex_said}");
    assert!(
        !alex_said.contains(JACKS_KEY),
        "another person's profile reached this run: {alex_said}"
    );
    assert!(
        !nobody_said.contains(JACKS_KEY) && !nobody_said.contains(ALEXS_KEY),
        "a session naming no profile was spawned with one: {nobody_said}"
    );

    harness.teardown().await;
}

/// The scripted agent rewrites its login the way a runtime refreshes one, and a second Session
/// is a fresh Instance: what it finds is what the first Run handed back.
#[tokio::test]
async fn a_login_refreshed_on_one_instance_is_the_one_the_next_instance_starts_from() {
    let harness = playing(Script::Refreshes).await;
    declared(&harness, "opencode").await;
    a_profile(&harness, "jack", "Jack", &login_file(), FIRST_LOGIN).await;

    let first = harness
        .open_session_with("acme", repository::NAME, "builder", "jack")
        .await;
    let (run, said) = worked(&harness, &first).await;
    assert_eq!(run.exit, Some(Exit::Succeeded), "{said}");
    assert!(
        said.contains(&format!("logged in as {FIRST_LOGIN}")),
        "{said}"
    );

    let second = harness
        .open_session_with("acme", repository::NAME, "builder", "jack")
        .await;
    let (next, said) = worked(&harness, &second).await;

    assert_ne!(
        next.instance, run.instance,
        "the second session reused an instance"
    );
    assert!(
        said.contains(&format!("logged in as {FIRST_LOGIN}{REFRESHED}")),
        "the refreshed login did not reach the next instance: {said}"
    );

    harness.teardown().await;
}

/// An Instance outlives its Run and holds on to what was written into it, so the login is
/// taken back out as the Run ends.
#[tokio::test]
async fn an_instance_holds_no_login_once_its_run_has_ended() {
    let harness = playing(Script::Refreshes).await;
    declared(&harness, "opencode").await;
    a_profile(&harness, "jack", "Jack", &login_file(), FIRST_LOGIN).await;
    let session = harness
        .open_session_with("acme", repository::NAME, "builder", "jack")
        .await;

    let (run, said) = worked(&harness, &session).await;

    assert_eq!(run.exit, Some(Exit::Succeeded), "{said}");
    let instance = run
        .instance
        .as_deref()
        .and_then(|instance| instance.strip_prefix("local-exec/"))
        .expect("a local instance");
    let home = std::env::temp_dir().join(format!("{instance}.home"));
    assert!(home.is_dir(), "the instance has no home of its own");
    assert!(
        !home.join(LOGIN).exists(),
        "the login was left on the instance after its run"
    );

    harness.teardown().await;
}

/// Nothing of what the profile holds is said anywhere a Session is read from.
#[tokio::test]
async fn a_run_spawned_with_a_profile_records_it_nowhere() {
    let harness = playing(Script::Speaks).await;
    declared(&harness, "opencode").await;
    a_profile(&harness, "jack", "Jack", &subscription_key(), JACKS_KEY).await;
    harness
        .hold_in_profile("acme", "jack", &login_file(), FIRST_LOGIN)
        .await;
    let session = harness
        .open_session_with("acme", repository::NAME, "builder", "jack")
        .await;

    let (run, said) = worked(&harness, &session).await;

    assert_eq!(run.exit, Some(Exit::Succeeded), "{said}");
    for secret in [JACKS_KEY, FIRST_LOGIN] {
        assert!(!said.contains(secret), "{said}");
        assert!(!format!("{run:?}").contains(secret));
        assert!(!format!("{session:?}").contains(secret));
    }

    harness.teardown().await;
}

#[tokio::test]
async fn a_profile_is_sealed_beside_the_database_and_listed_by_name_never_by_value() {
    let harness = Harness::boot().await;
    declared(&harness, "opencode").await;
    a_profile(&harness, "jack", "Jack", &subscription_key(), JACKS_KEY).await;
    harness
        .hold_in_profile("acme", "jack", &login_file(), FIRST_LOGIN)
        .await;

    let listed = harness.profiles("acme").await;

    let [(profile, held)] = &listed[..] else {
        panic!("the organization lists {listed:?}");
    };
    assert_eq!(profile.owner, "Jack");
    assert_eq!(
        held.iter()
            .map(|held| held.entry.to_string())
            .collect::<Vec<_>>(),
        [
            format!("file {LOGIN}"),
            format!("variable {SUBSCRIPTION_KEY}")
        ]
    );
    assert!(!format!("{listed:?}").contains(JACKS_KEY));
    for kept in ["kestrel.db", "kestrel.db-wal"] {
        let Ok(written) = std::fs::read(harness.data_dir().join(kept)) else {
            continue;
        };
        for secret in [JACKS_KEY, FIRST_LOGIN] {
            assert!(
                !written
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "a copy of {kept} is a usable login"
            );
        }
    }

    harness.teardown().await;
}

#[tokio::test]
async fn a_profile_never_changes_hands() {
    let harness = Harness::boot().await;
    declared(&harness, "opencode").await;
    harness
        .declare_profile("acme", "jack", "Jack")
        .await
        .expect("the profile should declare");

    let taken = harness.declare_profile("acme", "jack", "Alex").await;

    assert!(
        taken
            .expect_err("the profile changed hands")
            .to_string()
            .contains("belongs to Jack")
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_whose_profile_holds_no_login_fails_before_an_instance() {
    let harness = playing(Script::Confides).await;
    declared(&harness, "opencode").await;
    harness
        .declare_profile("acme", "jack", "Jack")
        .await
        .expect("the profile should declare");
    let session = harness
        .open_session_with("acme", repository::NAME, "builder", "jack")
        .await;

    let (run, _) = worked(&harness, &session).await;

    let Some(Exit::Failed { because }) = &run.exit else {
        panic!("the run ended {:?} on a profile holding nothing", run.exit);
    };
    assert!(because.contains("holds no login"), "{because}");
    assert!(run.instance.is_none());

    harness.teardown().await;
}

/// Two copies of one rotating login race to refresh it, so the second Run waits for the first.
#[tokio::test]
async fn runs_on_a_serialized_runtime_sharing_a_profile_are_dispatched_one_at_a_time() {
    let harness = Harness::boot().await;
    declared(&harness, SERIALIZED).await;
    let organization = harness.organizations().await.remove(0);
    harness
        .declare_agent(&organization, "reviewer", "claude", None)
        .await;
    a_profile(&harness, "jack", "Jack", &login_file(), FIRST_LOGIN).await;
    a_profile(&harness, "alex", "Alex", &login_file(), FIRST_LOGIN).await;
    let open = |agent: &'static str, profile: &'static str| {
        harness.open_session_with("acme", repository::NAME, agent, profile)
    };
    let (jacks, jacks_again, alexs, jacks_other_runtime) = (
        open("builder", "jack").await,
        open("builder", "jack").await,
        open("builder", "alex").await,
        open("reviewer", "jack").await,
    );
    for session in [&jacks, &jacks_again, &alexs, &jacks_other_runtime] {
        harness.enqueue_run(session.id).await;
    }

    let mut claimed = Vec::new();
    while let Some(next) = harness.claim_run().await {
        claimed.push(next.run);
    }

    let sessions: Vec<_> = claimed.iter().map(|run| run.session).collect();
    assert_eq!(sessions, [jacks.id, alexs.id, jacks_other_runtime.id]);

    harness.complete_run(&claimed[0]).await;
    assert_eq!(
        harness.claim_run().await.map(|next| next.run.session),
        Some(jacks_again.id)
    );

    harness.teardown().await;
}

/// A Run can hand back a refreshed login and never add one the person did not put there.
#[tokio::test]
async fn a_run_refreshes_only_the_files_its_profile_already_holds() {
    let harness = Harness::boot().await;
    declared(&harness, "opencode").await;
    a_profile(&harness, "jack", "Jack", &login_file(), FIRST_LOGIN).await;
    let session = harness
        .open_session_with("acme", repository::NAME, "builder", "jack")
        .await;
    let (run, credential) = harness.dispatch_run(session.id).await;

    let answered = Link::to(&harness.link())
        .refresh(
            run.id,
            &credential,
            &[
                (LOGIN, "a-refreshed-login"),
                (".ssh/id_ed25519", "smuggled"),
            ],
        )
        .await;

    assert_eq!(answered.status(), StatusCode::NO_CONTENT);
    let profile = session.profile.expect("the session names a profile");
    let contents = harness.profile_contents(&profile).await;
    assert_eq!(
        contents.files.into_iter().collect::<Vec<_>>(),
        [(LOGIN.to_owned(), "a-refreshed-login".to_owned())]
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_that_has_ended_refreshes_nothing() {
    let harness = Harness::boot().await;
    declared(&harness, "opencode").await;
    a_profile(&harness, "jack", "Jack", &login_file(), FIRST_LOGIN).await;
    let session = harness
        .open_session_with("acme", repository::NAME, "builder", "jack")
        .await;
    let (run, credential) = harness.dispatch_run(session.id).await;
    harness.complete_run(&run).await;

    let answered = Link::to(&harness.link())
        .refresh(run.id, &credential, &[(LOGIN, "too-late")])
        .await;

    assert_eq!(answered.status(), StatusCode::UNAUTHORIZED);
    let profile = session.profile.expect("the session names a profile");
    assert_eq!(
        harness.profile_contents(&profile).await.files[LOGIN],
        FIRST_LOGIN
    );

    harness.teardown().await;
}
