//! The rule that turns an Event into work (0.1/21). Labelling an issue opens a Session with
//! the Event as its first Transcript entry and a Run queued behind it, with nobody in the
//! loop; a Trigger fires at most once per Event, never for an Event recorded before it was
//! declared, and one that is disabled fires for nothing.

mod support;

use std::time::Duration;

use jiff::SignedDuration;
use kestrel::domain::{Direction, Event, RunState, Session, TriggerState};
use kestrel::log::Entry;
use support::github_stub::{self, GithubStub};
use support::{Harness, labelled_on};

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
async fn an_organization(harness: &Harness, name: &str) {
    let organization = harness.declare_organization(name).await;
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
        .declare_trigger(
            "acme",
            "ready",
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "builder",
        )
        .await;
}

async fn opened(harness: &Harness, count: usize) -> Vec<Session> {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let sessions = harness.sessions("acme").await;
        if sessions.len() >= count {
            return sessions;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{count} sessions were never opened, only {}",
            sessions.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn recorded(harness: &Harness, count: usize) -> Vec<Event> {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let events = harness.events("acme").await;
        if events.len() >= count {
            return events;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the repository's events were never recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Nothing opening is only observable by waiting for the sweeps that would have opened it, so
/// this waits for the Event to be recorded and then for several sweeps to pass over it.
async fn nothing_opens(harness: &Harness) {
    recorded(harness, 1).await;
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
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let session = opened(&harness, 1).await.remove(0);

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
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let session = opened(&harness, 1).await.remove(0);
    let transcript = harness.transcript(session.id).await;

    let Entry::TriggerFired {
        trigger,
        occurrence,
    } = &transcript
        .first()
        .expect("a triggered session has a transcript")
        .entry
    else {
        panic!("the first entry is {}", transcript[0].entry);
    };
    assert_eq!(trigger, "ready");
    assert_eq!(
        occurrence.source,
        format!("https://github.com/{REPOSITORY}")
    );
    let data = kestrel::integration::github::EventData::new(occurrence);
    assert_eq!(data.subject_issue(), Some(43));
    assert_eq!(data.label(), Some(READY));
    assert_eq!(data.actor(), Some("jtmthf"));
    assert_eq!(data.title(), Some("an issue numbered 43"));

    harness.teardown().await;
}

#[tokio::test]
async fn the_session_records_the_event_that_started_it() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let session = opened(&harness, 1).await.remove(0);
    let events = harness.events("acme").await;

    assert_eq!(session.started_by, Some(events[0].record_id));

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
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    opened(&harness, 1).await;
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
async fn an_event_matching_several_triggers_fires_every_one_of_them() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    harness
        .declare_trigger(
            "acme",
            "anything-labelled",
            r#"{"exact": {"type": "com.github.issues.labeled"}}"#,
            "kestrel",
            "builder",
        )
        .await;
    watching(&harness, &stub).await;

    let sessions = opened(&harness, 2).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut fired = Vec::new();
    for session in harness.sessions("acme").await {
        if let Entry::TriggerFired { trigger, .. } = &harness.transcript(session.id).await[0].entry
        {
            fired.push(trigger.clone());
        }
    }
    fired.sort();
    assert_eq!(sessions.len(), 2);
    assert_eq!(fired, ["anything-labelled", "ready"]);

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
    an_organization(&harness, "acme").await;
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
    an_organization(&harness, "acme").await;
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
    an_organization(&harness, "acme").await;
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
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let listed = harness.triggers("acme").await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "ready");
    assert_eq!(
        listed[0].filter.to_string(),
        r#"source = "https://github.com/jtmthf/kestrel" and type = "com.github.issues.labeled" and data.label.name = "ready-for-agent""#
    );
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
    an_organization(&harness, "acme").await;
    watching(&harness, &stub).await;

    recorded(&harness, 3).await;
    ready_for_agent(&harness).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_trigger_fires_only_for_the_source_it_names() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    harness
        .declare_trigger(
            "acme",
            "elsewhere",
            &labelled_on("globex/other", READY),
            "kestrel",
            "builder",
        )
        .await;
    watching(&harness, &stub).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

/// A dry run asks only whether the filter matches, so it answers for an Event recorded before
/// the Trigger was declared, which is the one kind a Trigger never fires for.
#[tokio::test]
async fn trigger_test_says_whether_a_recorded_event_matches() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    watching(&harness, &stub).await;
    let event = recorded(&harness, 1).await.remove(0).record_id;

    for (at, (filter, matches)) in [
        (r#"{"exact": {"type": "com.github.issues.labeled"}}"#, true),
        (
            r#"{"exact": {"type": "com.github.issues.unlabeled"}}"#,
            false,
        ),
        (r#"{"exact": {"type": "com.github.issues"}}"#, false),
        (
            r#"{"prefix": {"source": "https://github.com/jtmthf/"}}"#,
            true,
        ),
        (
            r#"{"prefix": {"source": "https://github.com/globex/"}}"#,
            false,
        ),
        (r#"{"suffix": {"subject": "43"}}"#, true),
        (r#"{"suffix": {"subject": "44"}}"#, false),
        (r#"{"exact": {"data.label.name": "ready-for-agent"}}"#, true),
        (r#"{"prefix": {"data.label.name": "ready-"}}"#, true),
        (r#"{"prefix": {"data.label.name": "READY-"}}"#, false),
        (r#"{"prefix": {"data.label.name": "ready_"}}"#, false),
        (r#"{"suffix": {"data.label.name": "%agent"}}"#, false),
        (r#"{"exact": {"data.issue.number": "43"}}"#, true),
        (r#"{"exact": {"data.actor": "jtmthf"}}"#, false),
        (r#"{"exact": {"data.milestone.title": "v1"}}"#, false),
        (
            r#"{"not": {"exact": {"data.milestone.title": "v1"}}}"#,
            true,
        ),
        (
            r#"{"not": {"exact": {"type": "com.github.issues.labeled"}}}"#,
            false,
        ),
        (
            r#"{"all": [
                {"exact": {"type": "com.github.issues.labeled"}},
                {"exact": {"data.label.name": "needs-triage"}}
            ]}"#,
            false,
        ),
        (
            r#"{"any": [
                {"exact": {"data.label.name": "needs-triage"}},
                {"exact": {"data.label.name": "ready-for-agent"}}
            ]}"#,
            true,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("case-{at}");
        harness
            .declare_trigger("acme", &name, filter, "kestrel", "builder")
            .await;

        assert_eq!(
            harness.test_trigger("acme", &name, event).await,
            matches,
            "{filter} should {}match the labelled event",
            if matches { "" } else { "not " }
        );
    }

    harness.teardown().await;
}

/// An Event belongs to one Organization, and another Organization's Trigger cannot so much as
/// ask whether it would have matched.
#[tokio::test]
async fn a_trigger_is_tested_only_against_its_own_organizations_events() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    an_organization(&harness, "globex").await;
    harness
        .declare_trigger(
            "globex",
            "ready",
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "builder",
        )
        .await;
    watching(&harness, &stub).await;
    let event = recorded(&harness, 1).await.remove(0).record_id;

    let refusal = harness
        .try_test_trigger("globex", "ready", event)
        .await
        .expect_err("another organization's event should be refused");

    assert!(
        refusal.to_string().contains("globex"),
        "unhelpful refusal: {refusal}"
    );

    harness.teardown().await;
}
