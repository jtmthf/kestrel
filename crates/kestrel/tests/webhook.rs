//! A webhook records a CloudEvent over HTTP: authenticated first, recorded, and answered without
//! any matching in between.

mod support;

use std::time::Duration;

use hmac::{Hmac, KeyInit as _, Mac as _};
use kestrel::domain::{Event, Integration};
use kestrel::integration::webhook::WRAPPED;
use reqwest::StatusCode;
use sha2::Sha256;
use support::github_stub::GithubStub;
use support::{Kestrel, labelled_on};

const PATIENCE: Duration = Duration::from_secs(30);
const SECRET: &str = "a-shared-secret";
const REPOSITORY: &str = "jtmthf/kestrel";

async fn a_webhook(kestrel: &Kestrel) -> Integration {
    kestrel.declare_organization("acme").await;
    kestrel.register_webhook("acme", "ci", SECRET).await
}

fn post(kestrel: &Kestrel, integration: &Integration) -> reqwest::RequestBuilder {
    reqwest::Client::new().post(format!("{}{}", kestrel.link(), integration.webhook_path()))
}

fn signature(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("an HMAC key");
    mac.update(body);
    let digest = mac.finalize().into_bytes();

    format!(
        "sha256={}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn labelled(issue: i64) -> Vec<u8> {
    serde_json::json!({
        "action": "labeled",
        "label": { "name": "ready-for-agent" },
        "issue": { "number": issue, "title": "Fix the thing" },
        "repository": { "full_name": REPOSITORY },
        "sender": { "login": "jtmthf" }
    })
    .to_string()
    .into_bytes()
}

fn delivered(
    kestrel: &Kestrel,
    integration: &Integration,
    delivery: &str,
    body: Vec<u8>,
    signed_with: &str,
) -> reqwest::RequestBuilder {
    post(kestrel, integration)
        .header("content-type", "application/json")
        .header("x-github-event", "issues")
        .header("x-github-delivery", delivery)
        .header("x-hub-signature-256", signature(signed_with, &body))
        .body(body)
}

async fn only_event(kestrel: &Kestrel) -> Event {
    let events = kestrel.events("acme").await;
    assert_eq!(
        events.len(),
        1,
        "exactly one event was recorded: {events:?}"
    );
    events.into_iter().next().expect("one event")
}

#[tokio::test]
async fn a_binary_cloudevent_is_recorded_as_its_sender_named_it() {
    let kestrel = Kestrel::boot().await;
    let webhook = a_webhook(&kestrel).await;

    let answered = post(&kestrel, &webhook)
        .bearer_auth(SECRET)
        .header("content-type", "application/json")
        .header("ce-specversion", "1.0")
        .header("ce-id", "build-7")
        .header("ce-source", "https://ci.example.com/pipelines/3")
        .header("ce-type", "com.example.build.failed")
        .body(r#"{"step": "test"}"#)
        .send()
        .await
        .expect("the webhook answers");

    assert_eq!(answered.status(), StatusCode::ACCEPTED);
    let event = only_event(&kestrel).await;
    assert_eq!(event.occurrence.id, "build-7");
    assert_eq!(
        event.occurrence.source,
        "https://ci.example.com/pipelines/3"
    );
    assert_eq!(event.occurrence.r#type, "com.example.build.failed");
    assert_eq!(event.occurrence.data, serde_json::json!({"step": "test"}));
    assert_eq!(event.integration, Some(webhook.id));

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_structured_cloudevent_is_recorded_once_however_often_it_is_delivered() {
    let kestrel = Kestrel::boot().await;
    let webhook = a_webhook(&kestrel).await;
    let envelope = serde_json::json!({
        "specversion": "1.0",
        "id": "deploy-1",
        "source": "/argo/sensors/deploy",
        "type": "io.argoproj.deployed",
        "subject": "kestrel",
        "data": {"image": "kestrel:1"}
    });

    for _ in 0..2 {
        let answered = post(&kestrel, &webhook)
            .bearer_auth(SECRET)
            .header("content-type", "application/cloudevents+json")
            .body(envelope.to_string())
            .send()
            .await
            .expect("the webhook answers");
        assert_eq!(answered.status(), StatusCode::ACCEPTED);
    }

    let event = only_event(&kestrel).await;
    assert_eq!(event.occurrence.id, "deploy-1");
    assert_eq!(event.occurrence.source, "/argo/sensors/deploy");
    assert_eq!(event.occurrence.r#type, "io.argoproj.deployed");
    assert_eq!(event.occurrence.subject.as_deref(), Some("kestrel"));

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_post_that_is_no_cloudevent_is_wrapped() {
    let kestrel = Kestrel::boot().await;
    let webhook = a_webhook(&kestrel).await;

    let answered = post(&kestrel, &webhook)
        .bearer_auth(SECRET)
        .header("content-type", "application/json")
        .body(r#"{"status": "green"}"#)
        .send()
        .await
        .expect("the webhook answers");

    assert_eq!(answered.status(), StatusCode::ACCEPTED);
    let event = only_event(&kestrel).await;
    assert_eq!(event.occurrence.r#type, WRAPPED);
    assert_eq!(event.occurrence.source, webhook.webhook_path());
    assert_eq!(
        event.occurrence.data,
        serde_json::json!({"status": "green"})
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_delivery_that_does_not_authenticate_is_refused_and_leaves_no_event() {
    let kestrel = Kestrel::boot().await;
    let webhook = a_webhook(&kestrel).await;

    let wrong = post(&kestrel, &webhook)
        .bearer_auth("not-the-secret")
        .body("hello")
        .send()
        .await
        .expect("the webhook answers");
    let absent = post(&kestrel, &webhook)
        .body("hello")
        .send()
        .await
        .expect("the webhook answers");
    let nowhere = reqwest::Client::new()
        .post(format!("{}/webhooks/not-an-integration", kestrel.link()))
        .bearer_auth(SECRET)
        .body("hello")
        .send()
        .await
        .expect("the webhook answers");

    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(absent.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(nowhere.status(), StatusCode::UNAUTHORIZED);
    assert!(kestrel.events("acme").await.is_empty());

    kestrel.teardown().await;
}

#[tokio::test]
async fn an_oversized_payload_is_refused_without_being_stored() {
    let kestrel = Kestrel::boot().await;
    let webhook = a_webhook(&kestrel).await;

    let answered = post(&kestrel, &webhook)
        .bearer_auth(SECRET)
        .header("content-type", "text/plain")
        .body("x".repeat(1024 * 1024 + 1))
        .send()
        .await
        .expect("the webhook answers");

    assert_eq!(answered.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(kestrel.events("acme").await.is_empty());
    let refusal = kestrel.integrations("acme").await[0]
        .last_event_refusal
        .clone();
    assert!(refusal.is_some(), "the refusal is visible to an operator");

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_github_delivery_is_verified_by_its_signature() {
    let kestrel = Kestrel::boot().await;
    let stub = GithubStub::start();
    kestrel.declare_organization("acme").await;
    let github = kestrel
        .register_signed_github("acme", "github", REPOSITORY, &stub.base_url(), SECRET)
        .await;

    let forged = delivered(&kestrel, &github, "d-1", labelled(43), "a-guess")
        .send()
        .await
        .expect("the webhook answers");
    assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);
    assert!(kestrel.events("acme").await.is_empty());

    let signed = delivered(&kestrel, &github, "d-2", labelled(43), SECRET)
        .send()
        .await
        .expect("the webhook answers");
    assert_eq!(signed.status(), StatusCode::ACCEPTED);
    let event = only_event(&kestrel).await;
    assert_eq!(event.occurrence.r#type, "com.github.issues.labeled");
    assert_eq!(
        event.occurrence.source,
        format!("https://github.com/{REPOSITORY}")
    );
    assert_eq!(event.occurrence.subject.as_deref(), Some("#43"));

    kestrel.teardown().await;
}

/// A shared secret is not how GitHub proves a delivery, and a polled integration has no key to
/// check a signature with.
#[tokio::test]
async fn a_github_integration_without_a_signing_secret_accepts_no_delivery() {
    let kestrel = Kestrel::boot().await;
    let stub = GithubStub::start();
    kestrel.declare_organization("acme").await;
    let polled = kestrel
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            &[kestrel::domain::Direction::Outbound],
            jiff::SignedDuration::from_secs(60),
        )
        .await;

    let answered = delivered(&kestrel, &polled, "d-1", labelled(43), SECRET)
        .bearer_auth(SECRET)
        .send()
        .await
        .expect("the webhook answers");

    assert_eq!(answered.status(), StatusCode::UNAUTHORIZED);
    assert!(kestrel.events("acme").await.is_empty());

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_delivered_label_opens_a_workspace_and_the_repository_is_not_polled() {
    let kestrel = Kestrel::boot().await;
    let stub = GithubStub::start();
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(
            &organization,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    kestrel
        .declare_agent(&organization, "builder", "opencode", None)
        .await;
    kestrel
        .declare_trigger(
            "acme",
            "ready",
            &labelled_on(REPOSITORY, "ready-for-agent"),
            "kestrel",
            "builder",
        )
        .await;
    let github = kestrel
        .register_signed_github("acme", "github", REPOSITORY, &stub.base_url(), SECRET)
        .await;

    let answered = delivered(&kestrel, &github, "d-1", labelled(43), SECRET)
        .send()
        .await
        .expect("the webhook answers");
    assert_eq!(answered.status(), StatusCode::ACCEPTED);

    let deadline = tokio::time::Instant::now() + PATIENCE;
    while kestrel.workspaces("acme").await.is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the delivered label never opened a workspace"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let requests = stub.requests();
    assert!(
        requests.iter().all(|request| {
            !request.url.contains("/issues/events?") && !request.url.contains("/issues/comments?")
        }),
        "a webhook-delivered repository was polled: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|request| request.url.ends_with("/issues/43")),
        "the issue was not checked before start: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|request| request.url.contains("/issues/43/dependencies/blocked_by?")),
        "the issue's blockers were not checked before start: {requests:?}"
    );

    kestrel.teardown().await;
}
