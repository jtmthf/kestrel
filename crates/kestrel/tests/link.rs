//! The link an Environment dials out to, driven through the primary test seam: a local-exec
//! Environment running the real supervisor binary, and a client of kestrel's own in the
//! test's hands for the answers a supervisor only ever reacts to.

mod support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{Exit, Run, RunId};
use kestrel::link::credential::Secret;
use kestrel::link::{self, Instruction, Report};
use kestrel::log::Entry;
use reqwest::{StatusCode, Version, header};
use serde_json::json;
use support::Harness;
use support::link_client::{Link, Next};
use support::supervisor::Supervisor;

const PATIENCE: Duration = Duration::from_secs(30);
const LONG_ENOUGH_TO_BE_SURE: Duration = Duration::from_millis(500);

async fn a_run(harness: &Harness) -> (Run, Secret) {
    declared(harness).await;

    another_run(harness).await
}

async fn declared(harness: &Harness) {
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
        .declare_agent(&organization, "builder", "opencode", "claude-opus-5")
        .await;
}

/// A second Session, because at 0.1 nothing yet stops two Runs being live in one.
async fn another_run(harness: &Harness) -> (Run, Secret) {
    let session = harness.open_session("acme", "kestrel", "builder").await;

    harness.dispatch_run(session.id).await
}

#[tokio::test]
async fn an_environment_dials_out_and_the_control_plane_knows_it_is_connected() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;

    assert!(harness.run(run.id).await.connected.is_none());
    let mut supervisor = Supervisor::provision(&harness.link(), run.id, &credential);
    supervisor.wait_until_it_says("reported connected").await;

    let connected = harness
        .run(run.id)
        .await
        .connected
        .expect("the control plane should know an environment is on the link");
    assert!(
        !connected.version.is_empty(),
        "the control plane learned that something connected, but not what"
    );

    supervisor.destroy();
    harness.teardown().await;
}

#[tokio::test]
async fn an_environment_holds_the_stream_open_until_the_control_plane_tells_it_to_stop() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;

    let mut supervisor = Supervisor::provision(&harness.link(), run.id, &credential);
    supervisor.wait_until_it_says("link open").await;
    assert!(
        supervisor.is_still_running(LONG_ENOUGH_TO_BE_SURE).await,
        "the supervisor let go of the stream on its own. it said:\n{}",
        supervisor.everything_it_said()
    );

    harness.instruct(&run, Instruction::Stop).await;

    let status = supervisor.exits().await;
    assert!(
        status.success(),
        "the supervisor exited {status}. it said:\n{}",
        supervisor.everything_it_said()
    );
    assert!(supervisor.said("instruction stop"));

    harness.teardown().await;
}

#[tokio::test]
async fn a_supervisor_that_loses_the_stream_comes_back_and_is_handed_what_it_missed() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;

    let mut supervisor = Supervisor::provision(&harness.link(), run.id, &credential);
    supervisor.wait_until_it_says("reported connected").await;

    let stopped = harness.kill().await;
    supervisor.wait_until_it_says("lost the link").await;
    stopped.instruct(&run, Instruction::Stop).await;
    let harness = stopped.restart().await;

    let status = supervisor.exits().await;
    assert!(
        status.success(),
        "the supervisor exited {status}. it said:\n{}",
        supervisor.everything_it_said()
    );
    assert!(
        supervisor.said("instruction stop"),
        "the supervisor never received the instruction sent while it was away. it said:\n{}",
        supervisor.everything_it_said()
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_reconnect_carrying_a_cursor_is_not_handed_what_it_already_had() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;
    harness.instruct(&run, Instruction::Stop).await;

    let link = Link::to(&harness.link());
    let mut first = link.open(run.id, &credential, None).await;
    let Next::Event(delivered) = first.next_within(PATIENCE).await else {
        panic!("the stream never delivered the instruction that was waiting on it");
    };
    assert_eq!(delivered.id.as_deref(), Some("1"));
    assert_eq!(delivered.name.as_deref(), Some("stop"));

    let mut again = link.open(run.id, &credential, Some(1)).await;

    assert!(
        matches!(again.next_within(LONG_ENOUGH_TO_BE_SURE).await, Next::Quiet),
        "reconnecting with a cursor was handed an instruction it had already been given"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn the_link_refuses_an_environment_presenting_no_credential() {
    let harness = Harness::boot().await;
    let (run, _) = a_run(&harness).await;
    let link = Link::to(&harness.link());

    assert_eq!(
        link.instructions(run.id, None, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        link.report(
            run.id,
            None,
            &Report::Connected {
                version: "0.0.0".to_owned()
            }
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );

    harness.teardown().await;
}

#[tokio::test]
async fn the_link_refuses_an_environment_presenting_an_expired_credential() {
    let harness = Harness::boot().await;
    let (run, _) = a_run(&harness).await;
    let expired = harness
        .issue_credential(&run, Timestamp::now() - SignedDuration::from_secs(1))
        .await;

    let refused = Link::to(&harness.link())
        .instructions(run.id, Some(&expired), None)
        .await;

    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);

    harness.teardown().await;
}

#[tokio::test]
async fn the_link_refuses_an_environment_presenting_a_credential_belonging_to_another_run() {
    let harness = Harness::boot().await;
    let (run, _) = a_run(&harness).await;
    let (_, another) = another_run(&harness).await;

    let refused = Link::to(&harness.link())
        .instructions(run.id, Some(&another), None)
        .await;

    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    harness.teardown().await;
}

#[tokio::test]
async fn a_credential_stops_working_when_its_run_ends() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;
    let link = Link::to(&harness.link());
    assert_eq!(
        link.instructions(run.id, Some(&credential), None)
            .await
            .status(),
        StatusCode::OK
    );

    harness.complete_run(&run).await;

    assert_eq!(
        link.instructions(run.id, Some(&credential), None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );

    harness.teardown().await;
}

#[tokio::test]
async fn the_link_has_nothing_to_say_about_a_run_it_has_never_heard_of() {
    let harness = Harness::boot().await;
    let (_, credential) = a_run(&harness).await;

    let refused = Link::to(&harness.link())
        .instructions_for("not-a-run", Some(&credential), None)
        .await;

    assert_eq!(refused.status(), StatusCode::NOT_FOUND);

    harness.teardown().await;
}

#[tokio::test]
async fn the_link_is_plain_http_with_no_protocol_upgrade() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;

    let stream = Link::to(&harness.link())
        .instructions(run.id, Some(&credential), None)
        .await;

    assert_eq!(stream.status(), StatusCode::OK);
    assert_eq!(stream.version(), Version::HTTP_11);
    assert_eq!(
        stream
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|kind| kind.to_str().ok()),
        Some("text/event-stream")
    );
    assert!(stream.headers().get(header::UPGRADE).is_none());
    assert!(
        !stream
            .headers()
            .get(header::CONNECTION)
            .and_then(|connection| connection.to_str().ok())
            .is_some_and(|connection| connection.to_lowercase().contains("upgrade"))
    );

    harness.teardown().await;
}

#[test]
fn the_published_openapi_document_describes_the_link_the_control_plane_serves() {
    let document = published();

    assert_eq!(document["openapi"], "3.1.0");

    let described: Vec<(String, String)> = document["paths"]
        .as_object()
        .expect("an object of paths")
        .iter()
        .flat_map(|(path, operations)| {
            operations
                .as_object()
                .expect("an object of operations")
                .keys()
                .map(|method| (path.clone(), method.clone()))
                .collect::<Vec<_>>()
        })
        .collect();

    assert_eq!(
        described,
        vec![
            (link::ENTRIES.to_owned(), "get".to_owned()),
            (link::INSTRUCTIONS.to_owned(), "get".to_owned()),
            (link::REPORTS.to_owned(), "post".to_owned()),
        ]
    );
}

/// A report body written as the document describes it, so a field the control plane renamed
/// under the specification fails here rather than as a 422 an Environment cannot act on.
#[tokio::test]
async fn the_link_takes_every_report_the_published_openapi_document_describes() {
    let described = reports_the_document_describes();
    let bodies = json!({
        "connected": {"kind": "connected", "version": "0.0.0"},
        "started": {"kind": "started"},
        "said": {"kind": "said", "message": "what the agent said"},
        "used": {
            "kind": "used",
            "usage": {
                "context_used": 1_200,
                "context_size": 200_000,
                "cost": {"amount": 0.42, "currency": "USD"},
            },
        },
        "finished": {"kind": "finished", "exit": {"status": "succeeded"}},
    });
    assert_eq!(
        described,
        bodies
            .as_object()
            .expect("an object of bodies")
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    );

    let harness = Harness::boot().await;
    let link = Link::to(&harness.link());
    declared(&harness).await;

    for kind in described {
        let (run, credential) = another_run(&harness).await;
        assert_eq!(
            link.report_body(run.id, Some(&credential), &bodies[&kind])
                .await
                .status(),
            StatusCode::ACCEPTED,
            "the link would not take a {kind} report as the document describes it"
        );
    }

    harness.teardown().await;
}

fn published() -> serde_json::Value {
    let document = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi/link.json");

    serde_json::from_str(&fs::read_to_string(document).expect("a readable openapi document"))
        .expect("valid json")
}

fn reports_the_document_describes() -> Vec<String> {
    published()["components"]["schemas"]["Report"]["discriminator"]["mapping"]
        .as_object()
        .expect("an object of report kinds")
        .keys()
        .cloned()
        .collect()
}

/// As a supervisor seeding a cold Environment walks it.
async fn paged(link: &Link, run: &Run, credential: &Secret, window: usize) -> Vec<i64> {
    let mut walked = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let response = link
            .entries(run.id, Some(credential), cursor.as_deref(), Some(window))
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        let page: serde_json::Value = response.json().await.expect("a page of the transcript");
        let entries = page["entries"].as_array().expect("an array of entries");
        assert!(entries.len() <= window, "a read overran its window: {page}");

        walked.extend(
            entries
                .iter()
                .map(|entry| entry["seq"].as_i64().expect("a seq")),
        );
        cursor = page["cursor"].as_str().map(str::to_owned);

        if !page["more"].as_bool().expect("whether more are waiting") {
            return walked;
        }
    }
}

#[tokio::test]
async fn an_environment_reads_the_transcript_of_the_session_its_run_belongs_to_in_windows() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;
    for message in 1..=4 {
        harness.said(&run, &format!("message {message}")).await;
    }
    let link = Link::to(&harness.link());

    let first: serde_json::Value = link
        .entries(run.id, Some(&credential), None, Some(2))
        .await
        .json()
        .await
        .expect("a page of the transcript");

    assert_eq!(first["entries"].as_array().expect("entries").len(), 2);
    assert_eq!(first["entries"][0]["entry"]["kind"], "participant_joined");
    assert_eq!(first["more"], true);
    assert!(first["cursor"].is_string());
    assert_eq!(
        paged(&link, &run, &credential, 2).await,
        (1..=5).collect::<Vec<_>>()
    );

    harness.teardown().await;
}

#[tokio::test]
async fn the_link_refuses_a_cursor_that_names_no_position_in_the_transcript() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;
    let link = Link::to(&harness.link());

    for cursor in ["halfway-through", &format!("{}:99", run.session)] {
        assert_eq!(
            link.entries(run.id, Some(&credential), Some(cursor), None)
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "the link took the cursor {cursor} and started the walk over"
        );
    }

    harness.teardown().await;
}

#[tokio::test]
async fn the_link_refuses_a_window_wider_than_one_read_may_return() {
    let harness = Harness::boot().await;
    let (run, credential) = a_run(&harness).await;
    let link = Link::to(&harness.link());

    assert_eq!(
        link.entries(run.id, Some(&credential), None, Some(5_000))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );

    harness.teardown().await;
}

#[tokio::test]
async fn the_link_refuses_a_transcript_read_from_an_environment_presenting_no_credential() {
    let harness = Harness::boot().await;
    let (run, _) = a_run(&harness).await;
    let link = Link::to(&harness.link());

    assert_eq!(
        link.entries(run.id, None, None, None).await.status(),
        StatusCode::UNAUTHORIZED
    );

    harness.teardown().await;
}

/// The document is what a supervisor seeding a cold Environment reads the entries against, so
/// a kind or a field the Transcript grew and the document did not fails here.
#[test]
fn the_published_openapi_document_describes_every_transcript_entry_the_link_serves() {
    let document = published();
    let mapping = document["components"]["schemas"]["Entry"]["discriminator"]["mapping"]
        .as_object()
        .expect("an object of entry kinds");

    let served = [
        Entry::ParticipantJoined {
            participant: "builder".to_owned(),
        },
        Entry::RunStarted {
            run: RunId::generate(),
        },
        Entry::Said {
            participant: "builder".to_owned(),
            message: "what the agent said".to_owned(),
        },
        Entry::RunEnded {
            run: RunId::generate(),
            exit: Exit::Succeeded,
        },
    ];

    let mut kinds: Vec<String> = Vec::new();
    for entry in served {
        let entry = serde_json::to_value(&entry).expect("an entry");
        let kind = entry["kind"].as_str().expect("a kind").to_owned();
        let schema = mapping
            .get(&kind)
            .unwrap_or_else(|| panic!("the document describes no {kind} entry"))
            .as_str()
            .expect("a reference");

        for field in resolve(&document, schema)["required"]
            .as_array()
            .expect("an array of required fields")
        {
            let field = field.as_str().expect("a named field");
            assert!(
                entry.get(field).is_some(),
                "the document requires {field} on a {kind} entry, and the link does not serve it"
            );
        }
        kinds.push(kind);
    }

    assert_eq!(kinds, mapping.keys().cloned().collect::<Vec<_>>());
}

fn resolve<'a>(document: &'a serde_json::Value, reference: &str) -> &'a serde_json::Value {
    reference
        .trim_start_matches("#/")
        .split('/')
        .fold(document, |document, step| &document[step])
}
