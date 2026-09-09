//! The rule that turns an Event into work (0.1/21). Labelling an issue opens a Session with
//! the Event as its first Transcript entry and a Run queued behind it, with nobody in the
//! loop; a Trigger fires at most once per Event, never for an Event recorded before it was
//! declared, and one that is disabled fires for nothing.

mod support;

use std::time::Duration;

use jiff::SignedDuration;
use kestrel::domain::{Direction, RunState, Session, TriggerState};
use kestrel::log::Entry;
use support::Harness;
use support::github_stub::{self, GithubStub};

const PATIENCE: Duration = Duration::from_secs(30);
const REPOSITORY: &str = "jtmthf/kestrel";
const READY: &str = "ready-for-agent";
const BOTH: &[Direction] = &[Direction::Inbound, Direction::Outbound];

/// Sooner than the wheel's own sweep, so what paces these tests is the sweep rather than a
/// wait written into them.
fn eagerly() -> SignedDuration {
    SignedDuration::from_millis(1)
}

/// An organization with somewhere for work to happen and someone to do it. The Trigger is the
/// one thing each test declares for itself.
async fn an_organization(harness: &Harness) {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &organization,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    harness
        .declare_agent(&organization, "builder", "opencode", None)
        .await;
}

/// The poll that records what happens on the repository, started after the Trigger the test
/// is about: an Event recorded before a Trigger was declared fires nothing.
async fn watching(harness: &Harness, stub: &GithubStub) {
    harness
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            BOTH,
            eagerly(),
        )
        .await;
}

async fn ready_for_agent(harness: &Harness) {
    harness
        .declare_trigger("acme", "ready", (REPOSITORY, READY), "kestrel", "builder")
        .await;
}

async fn opened(harness: &Harness) -> Session {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let sessions = harness.sessions("acme").await;
        if let Some(session) = sessions.into_iter().next() {
            return session;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no session was ever opened"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Nothing opening is only observable by waiting for the sweeps that would have opened it, so
/// this waits for the Event to be recorded and then for several sweeps to pass over it.
async fn nothing_opens(harness: &Harness) {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    while harness.events("acme").await.is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the event was never recorded, so nothing was ever matched against"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        harness.sessions("acme").await.is_empty(),
        "a session was opened for an event nothing should have fired on"
    );
}

#[tokio::test]
async fn labelling_an_issue_opens_a_session_and_enqueues_a_run() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let session = opened(&harness).await;

    assert_eq!(session.workspace.name, "kestrel");
    assert_eq!(session.agent.name, "builder");

    let runs = harness.runs(session.id).await;
    assert_eq!(runs.len(), 1, "a firing enqueues one run");
    assert_eq!(runs[0].state, RunState::Queued);

    harness.teardown().await;
}

#[tokio::test]
async fn the_event_is_the_sessions_first_transcript_entry() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let session = opened(&harness).await;
    let transcript = harness.transcript(session.id).await;

    let Entry::TriggerFired {
        trigger,
        repository,
        occurrence,
    } = &transcript
        .first()
        .expect("a triggered session has a transcript")
        .entry
    else {
        panic!("the first entry is {}", transcript[0].entry);
    };
    assert_eq!(trigger, "ready");
    assert_eq!(repository, REPOSITORY);
    assert_eq!(occurrence.subject, 43);
    assert_eq!(occurrence.label.as_deref(), Some(READY));
    assert_eq!(occurrence.actor, "jtmthf");
    assert_eq!(occurrence.title, "an issue numbered 43");

    harness.teardown().await;
}

#[tokio::test]
async fn the_session_records_the_event_that_started_it() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let session = opened(&harness).await;
    let events = harness.events("acme").await;

    assert_eq!(session.started_by, Some(events[0].id));

    harness.teardown().await;
}

/// The same label going on the same issue twice is one Event however many polls see it, and
/// one firing however many sweeps pass over it.
#[tokio::test]
async fn relabelling_the_same_issue_twice_opens_exactly_one_session() {
    let stub = GithubStub::start();
    let relabelled = github_stub::page(&[github_stub::labelled(7, 43, READY)]);
    stub.script(relabelled.clone());
    stub.script(relabelled);
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    opened(&harness).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let sessions = harness.sessions("acme").await;
    assert_eq!(
        sessions.len(),
        1,
        "one event opened {} sessions",
        sessions.len()
    );

    harness.teardown().await;
}

#[tokio::test]
async fn an_event_matching_no_trigger_opens_nothing() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(
        7,
        43,
        "needs-triage",
    )]));
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

/// GitHub reports the label coming off as an `unlabeled` event carrying that same label, and
/// a trigger that fired on it would start work every time someone tidied an issue up.
#[tokio::test]
async fn taking_the_label_back_off_fires_nothing() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::unlabelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_disabled_trigger_fires_for_nothing() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    ready_for_agent(&harness).await;
    harness.disable_trigger("acme", "ready").await;
    watching(&harness, &stub).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

/// Disabling stops a Trigger firing without forgetting it, so what it was declared to match
/// is still there to be enabled again.
#[tokio::test]
async fn a_trigger_is_named_listed_and_disabled() {
    let stub = GithubStub::start();
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let listed = harness.triggers("acme").await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "ready");
    assert_eq!(listed[0].repository, REPOSITORY);
    assert_eq!(listed[0].label, READY);
    assert_eq!(listed[0].workspace.name, "kestrel");
    assert_eq!(listed[0].agent.name, "builder");
    assert_eq!(listed[0].state, TriggerState::Enabled);

    assert_eq!(
        harness.disable_trigger("acme", "ready").await.state,
        TriggerState::Disabled
    );
    assert_eq!(
        harness.show_trigger("acme", "ready").await.state,
        TriggerState::Disabled
    );
    assert_eq!(
        harness.enable_trigger("acme", "ready").await.state,
        TriggerState::Enabled
    );

    harness.teardown().await;
}

/// Turning automation on never works a backlog. A Trigger declared against a repository whose
/// history kestrel already holds opens nothing for that history, however many sweeps pass over
/// it; catching one up is a deliberate act, and declaring a Trigger is not it.
#[tokio::test]
async fn a_trigger_never_fires_for_events_recorded_before_it_was_declared() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[
        github_stub::labelled(9, 45, READY),
        github_stub::labelled(8, 44, READY),
        github_stub::labelled(7, 43, READY),
    ]));
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    watching(&harness, &stub).await;

    let deadline = tokio::time::Instant::now() + PATIENCE;
    while harness.events("acme").await.len() < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the repository's history was never recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    ready_for_agent(&harness).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_trigger_fires_only_for_the_repository_it_names() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness).await;
    harness
        .declare_trigger(
            "acme",
            "elsewhere",
            ("globex/other", READY),
            "kestrel",
            "builder",
        )
        .await;
    watching(&harness, &stub).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_trigger_names_a_repository_as_owner_and_name() {
    let harness = Harness::boot().await;
    an_organization(&harness).await;

    let refusal = harness
        .try_declare_trigger("acme", "ready", ("kestrel", READY), "kestrel", "builder")
        .await
        .expect_err("a repository that is not owner/name should be refused");

    assert!(
        refusal.to_string().contains("owner/name"),
        "unhelpful refusal: {refusal}"
    );

    harness.teardown().await;
}
