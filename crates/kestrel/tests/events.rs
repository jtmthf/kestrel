//! Discovering Events on a repository kestrel does not own, by polling rather than by webhook
//! (ADR-0005). An Event is identified and deduplicated, so an overlapping poll window records
//! each one exactly once, and a control plane that restarts carries on from where it stopped.

mod support;

use std::time::Duration;

use jiff::SignedDuration;
use kestrel::domain::{Direction, Event};
use support::github_stub::{self, GithubStub, ScriptedResponse};
use support::{Harness, TOKEN};

const PATIENCE: Duration = Duration::from_secs(30);
const REPOSITORY: &str = "jtmthf/kestrel";
const BOTH: &[Direction] = &[Direction::Inbound, Direction::Outbound];

/// Sooner than the wheel's own sweep, so what paces these tests is the sweep rather than a
/// wait written into them.
fn eagerly() -> SignedDuration {
    SignedDuration::from_millis(1)
}

async fn watching(harness: &Harness, stub: &GithubStub, carries: &[Direction]) {
    harness.declare_organization("acme").await;
    harness
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            carries,
            eagerly(),
        )
        .await;
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
            "{count} events were never recorded; {} were",
            events.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn polled(stub: &GithubStub, times: usize) {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let asked = stub.requests().len();
        if asked >= times {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "github was polled {asked} times rather than {times}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn labelled_ready(id: i64, issue: i64) -> serde_json::Value {
    github_stub::labelled(id, issue, "ready-for-agent")
}

#[tokio::test]
async fn events_on_a_watched_repository_are_recorded_and_listed() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[labelled_ready(7, 43)]));
    let harness = Harness::boot().await;
    watching(&harness, &stub, BOTH).await;

    let events = recorded(&harness, 1).await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].repository, REPOSITORY);
    assert_eq!(events[0].occurrence.kind, "labeled");
    assert_eq!(
        events[0].occurrence.label.as_deref(),
        Some("ready-for-agent")
    );
    assert_eq!(events[0].occurrence.subject, 43);
    assert_eq!(events[0].occurrence.actor, "jtmthf");

    harness.teardown().await;
}

#[tokio::test]
async fn an_integration_declares_which_directions_it_carries() {
    let harness = Harness::boot().await;
    let stub = GithubStub::start();
    watching(&harness, &stub, &[Direction::Inbound]).await;

    let registered = harness.integrations("acme").await;

    assert_eq!(registered.len(), 1);
    assert!(registered[0].carries(Direction::Inbound));
    assert!(!registered[0].carries(Direction::Outbound));

    harness.teardown().await;
}

/// The direction is what an Integration does rather than a label beside it: nothing polls one
/// that only carries kestrel's requests outward.
#[tokio::test]
async fn an_integration_that_carries_only_outbound_is_never_polled() {
    let outbound_only = GithubStub::start();
    let polling = GithubStub::start();
    let harness = Harness::boot().await;

    harness.declare_organization("acme").await;
    harness
        .register_integration(
            "acme",
            "outbound",
            REPOSITORY,
            &outbound_only.base_url(),
            &[Direction::Outbound],
            eagerly(),
        )
        .await;
    harness
        .register_integration(
            "acme",
            "inbound",
            REPOSITORY,
            &polling.base_url(),
            &[Direction::Inbound],
            eagerly(),
        )
        .await;

    polled(&polling, 3).await;

    assert!(
        outbound_only.requests().is_empty(),
        "an integration that carries nothing inbound was polled"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn two_polls_with_an_overlapping_window_record_each_event_once() {
    let stub = GithubStub::start();
    let overlapping = github_stub::page(&[labelled_ready(8, 44), labelled_ready(7, 43)]);
    stub.script(overlapping.clone());
    stub.script(overlapping);
    let harness = Harness::boot().await;
    watching(&harness, &stub, BOTH).await;

    recorded(&harness, 2).await;
    polled(&stub, 2).await;

    let events = harness.events("acme").await;
    assert_eq!(
        events.len(),
        2,
        "the same two events were recorded {} times over",
        events.len()
    );

    harness.teardown().await;
}

#[tokio::test]
async fn polling_resumes_after_a_restart_without_re_recording_what_it_already_saw() {
    let stub = GithubStub::start();
    let page = github_stub::page(&[labelled_ready(7, 43)]);
    stub.script(page.clone());
    let harness = Harness::boot().await;
    watching(&harness, &stub, BOTH).await;
    recorded(&harness, 1).await;

    let harness = harness.kill_and_restart().await;
    let asked = stub.requests().len();
    stub.script(page);
    polled(&stub, asked + 1).await;

    let events = harness.events("acme").await;
    assert_eq!(
        events.len(),
        1,
        "a restarted control plane recorded what it had already seen"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn the_poll_interval_survives_a_restart() {
    let stub = GithubStub::start();
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;
    harness
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            BOTH,
            SignedDuration::from_secs(97),
        )
        .await;

    let harness = harness.kill_and_restart().await;

    assert_eq!(
        harness.integrations("acme").await[0].interval,
        SignedDuration::from_secs(97)
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_rate_limited_poll_loses_no_event_and_records_none_twice() {
    let stub = GithubStub::start();
    stub.script(github_stub::rate_limited());
    stub.script(github_stub::page(&[labelled_ready(7, 43)]));
    let harness = Harness::boot().await;
    watching(&harness, &stub, BOTH).await;

    let events = recorded(&harness, 1).await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].occurrence.subject, 43);

    harness.teardown().await;
}

#[tokio::test]
async fn a_transient_failure_loses_no_event_and_records_none_twice() {
    let stub = GithubStub::start();
    stub.script(ScriptedResponse::answering(502));
    stub.script(github_stub::page(&[labelled_ready(7, 43)]));
    stub.script(github_stub::page(&[labelled_ready(7, 43)]));
    let harness = Harness::boot().await;
    watching(&harness, &stub, BOTH).await;

    recorded(&harness, 1).await;
    polled(&stub, 3).await;

    assert_eq!(harness.events("acme").await.len(), 1);

    harness.teardown().await;
}

/// The credential is the Organization's, and the only place it is ever spoken is the request
/// to the external system it was registered for.
#[tokio::test]
async fn the_organizations_credential_is_what_the_poll_presents() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[labelled_ready(7, 43)]));
    let harness = Harness::boot().await;
    watching(&harness, &stub, BOTH).await;

    recorded(&harness, 1).await;

    let asked = stub.requests();
    let authorization = asked[0]
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .map(|(_, value)| value.as_str())
        .expect("the poll should authenticate");
    assert_eq!(authorization, format!("Bearer {TOKEN}"));
    assert!(
        asked[0]
            .url
            .starts_with("/repos/jtmthf/kestrel/issues/events"),
        "the poll asked for {}",
        asked[0].url
    );

    harness.teardown().await;
}

#[tokio::test]
async fn an_integration_names_a_repository_as_owner_and_name() {
    let stub = GithubStub::start();
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;

    let refusal = harness
        .try_register_integration(
            "acme",
            "github",
            "kestrel",
            &stub.base_url(),
            BOTH,
            eagerly(),
        )
        .await
        .expect_err("a repository that is not owner/name should be refused");

    assert!(
        refusal.to_string().contains("owner/name"),
        "unhelpful refusal: {refusal}"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn an_integration_and_what_it_discovers_belong_to_one_organization() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[labelled_ready(7, 43)]));
    let harness = Harness::boot().await;
    watching(&harness, &stub, BOTH).await;
    harness.declare_organization("globex").await;

    recorded(&harness, 1).await;

    assert!(
        harness.integrations("globex").await.is_empty(),
        "another organization can see the integration acme registered"
    );
    assert!(
        harness.events("globex").await.is_empty(),
        "another organization can see the events acme's integration discovered"
    );

    harness.teardown().await;
}
