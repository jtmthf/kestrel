//! The link an Environment dials out to, driven through the primary test seam: a local-exec
//! Environment running the real supervisor binary, and a client of kestrel's own in the
//! test's hands for the answers a supervisor only ever reacts to.

mod support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::Run;
use kestrel::link::credential::Secret;
use kestrel::link::{self, Instruction, Report};
use reqwest::{StatusCode, Version, header};
use support::Harness;
use support::link_client::{Link, Next};
use support::supervisor::Supervisor;

const PATIENCE: Duration = Duration::from_secs(30);
const LONG_ENOUGH_TO_BE_SURE: Duration = Duration::from_millis(500);

async fn a_run(harness: &Harness) -> (Run, Secret) {
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
    another_run(harness).await
}

/// A second Session, because at 0.1 nothing yet stops two Runs being live in one.
async fn another_run(harness: &Harness) -> (Run, Secret) {
    let session = harness.open_session("acme", "kestrel", "builder").await;

    harness.start_run(session.id).await
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

    harness.end_run(&run).await;

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
    let document = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi/link.json");
    let document: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(document).expect("a readable openapi document"))
            .expect("valid json");

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
            (link::INSTRUCTIONS.to_owned(), "get".to_owned()),
            (link::REPORTS.to_owned(), "post".to_owned()),
        ]
    );
}
