mod support;

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use kestrel::domain::{EventRecordId, Exit, RunId};
use kestrel::link;
use kestrel::log::{Entry, Message};
use kestrel::operator;
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::client::{self, Client};
use support::github_stub::GithubStub;
use support::{Harness, TOKEN};

async fn an_open_session(harness: &Harness, said: usize) -> (String, kestrel::domain::Run) {
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
    let session = harness.open_session("acme", "kestrel", "builder").await;
    let (run, _) = harness.dispatch_run(session.id).await;
    for message in 1..=said {
        harness.said(&run, &format!("message {message}")).await;
    }

    (session.id.to_string(), run)
}

async fn recorded_seqs(harness: &Harness, session: &str) -> Vec<i64> {
    harness
        .transcript(session.parse().expect("a session id"))
        .await
        .iter()
        .map(|entry| entry.seq)
        .collect()
}

fn seqs(lines: &[String]) -> Vec<i64> {
    lines
        .iter()
        .map(|line| {
            let entry: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{line} is not an entry: {error}"));
            entry["seq"].as_i64().expect("a seq")
        })
        .collect()
}

fn records(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{line} is not a record: {error}"))
        })
        .collect()
}

fn generated_name(record: &Value) -> &str {
    let name = record["name"].as_str().expect("a generated name");
    let mut words = name.split('-');
    let adjective = words.next().expect("an adjective");
    let noun = words.next().expect("a noun");
    let suffix = words.next().expect("a generated suffix");
    assert!(!adjective.is_empty(), "a name has an adjective");
    assert!(!noun.is_empty(), "a name has a noun");
    assert_eq!(suffix.len(), 8, "a name has an eight-letter suffix");
    assert!(words.next().is_none(), "a name has no extra words");
    name
}

async fn client(harness: &Harness, args: &[&str]) -> client::Finished {
    client_given(harness, args, None).await
}

async fn client_given(harness: &Harness, args: &[&str], input: Option<&str>) -> client::Finished {
    let invocation = input.map_or_else(client::Invocation::default, |input| {
        client::Invocation::default().given(input)
    });
    client::ran_by(harness, args, invocation).await
}

fn succeeded(finished: &client::Finished) -> &[String] {
    assert!(
        finished.status.success(),
        "the client failed:\n{}",
        finished.err
    );
    assert!(
        finished.left_behind.is_empty(),
        "the client wrote {:?} where it ran",
        finished.left_behind
    );
    &finished.out
}

fn recorded(finished: &client::Finished) -> Vec<Value> {
    records(succeeded(finished))
}

/// What each test reads, named the way a script names it: a field the boundary gains later
/// reaches none of these assertions.
const ORGANIZATION: &str = "id,name";
const WORKSPACE: &str = "id,name,repositories,branch";
const AGENT: &str = "id,name,runtime,model";
const CREDENTIAL: &str = "variable";
const INTEGRATION: &str = "id,kind,repository,carries,polled_every,webhook_path,last_event_refusal";
const EVENT: &str = "record,integration,event";
const TRIGGER: &str = "id,name,state,brief";
const SESSION: &str = "id,name,state,continues";
const RUN: &str = "id,name,session,state,model";
const ENTRY: &str = "seq,entry";

/// Every answer is checked against what the published document says the operation answers.
async fn requested(
    harness: &Harness,
    method: reqwest::Method,
    path: &str,
    body: Option<&Value>,
) -> (StatusCode, Value) {
    let mut request =
        reqwest::Client::new().request(method.clone(), format!("{}{path}", harness.operator()));
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request
        .send()
        .await
        .expect("the operator boundary should answer");
    let status = response.status();
    let text = response.text().await.expect("an answer");
    let body = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{text} is not JSON: {error}"))
    };

    let (path, _) = path.split_once('?').unwrap_or((path, ""));
    conforms(path, &method.as_str().to_lowercase(), status, &body);
    (status, body)
}

async fn declared(harness: &Harness, path: &str, declaration: &Value) -> (StatusCode, Value) {
    requested(harness, reqwest::Method::POST, path, Some(declaration)).await
}

async fn got(harness: &Harness, path: &str) -> (StatusCode, Value) {
    requested(harness, reqwest::Method::GET, path, None).await
}

async fn listed(harness: &Harness, path: &str) -> Vec<Value> {
    let (status, body) = got(harness, path).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    body.as_array().expect("an array of records").clone()
}

async fn listed_nothing(harness: &Harness, path: &str) -> bool {
    listed(harness, path).await.is_empty()
}

fn workspaces_of(organization: &str) -> String {
    operator::WORKSPACES.replace("{organization}", organization)
}

fn agents_of(organization: &str) -> String {
    operator::AGENTS.replace("{organization}", organization)
}

fn credentials_of(organization: &str) -> String {
    operator::CREDENTIALS.replace("{organization}", organization)
}

fn credential_of(organization: &str, variable: &str) -> String {
    operator::CREDENTIAL
        .replace("{organization}", organization)
        .replace("{variable}", variable)
}

fn integrations_of(organization: &str) -> String {
    operator::INTEGRATIONS.replace("{organization}", organization)
}

fn event_refusal_of(organization: &str, integration: &str) -> String {
    operator::EVENT_REFUSAL
        .replace("{organization}", organization)
        .replace("{integration}", integration)
}

fn events_of(organization: &str) -> String {
    operator::EVENTS.replace("{organization}", organization)
}

fn event_at(record: &str) -> String {
    operator::EVENT.replace("{record}", record)
}

fn triggers_of(organization: &str) -> String {
    operator::TRIGGERS.replace("{organization}", organization)
}

fn trigger_at(organization: &str, trigger: &str) -> String {
    operator::TRIGGER
        .replace("{organization}", organization)
        .replace("{trigger}", trigger)
}

fn failed(finished: &client::Finished) -> &str {
    assert!(
        !finished.status.success(),
        "the client was expected to be refused, and printed {:?}",
        finished.out
    );
    &finished.err
}

#[tokio::test]
async fn a_client_declares_and_lists_organizations_without_opening_a_database() {
    let harness = Harness::boot().await;

    let declared = recorded(
        &client(
            &harness,
            &["organization", "declare", "acme", "--json", ORGANIZATION],
        )
        .await,
    );
    succeeded(&client(&harness, &["organization", "declare", "globex"]).await);
    let listed =
        recorded(&client(&harness, &["organization", "list", "--json", ORGANIZATION]).await);

    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0]["name"], "acme");
    assert_eq!(
        listed
            .iter()
            .map(|organization| organization["name"].as_str().expect("a name"))
            .collect::<Vec<_>>(),
        vec!["acme", "globex"]
    );
    assert_eq!(listed[0]["id"], declared[0]["id"]);
    assert_eq!(
        harness.organizations().await[0].id.to_string(),
        declared[0]["id"].as_str().expect("an id")
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_declares_and_lists_workspaces_and_agents() {
    let harness = Harness::boot().await;
    succeeded(&client(&harness, &["organization", "declare", "acme"]).await);

    let workspace = recorded(
        &client(
            &harness,
            &[
                "workspace",
                "declare",
                "kestrel",
                "--organization",
                "acme",
                "--repository",
                "https://github.com/jtmthf/kestrel",
                "--repository",
                "https://github.com/jtmthf/skills",
                "--branch",
                "main",
                "--json",
                WORKSPACE,
            ],
        )
        .await,
    );
    let agent = recorded(
        &client(
            &harness,
            &[
                "agent",
                "declare",
                "builder",
                "--organization",
                "acme",
                "--model",
                "claude-opus-5",
                "--json",
                AGENT,
            ],
        )
        .await,
    );
    let workspaces = recorded(
        &client(
            &harness,
            &[
                "workspace",
                "list",
                "--organization",
                "acme",
                "--json",
                WORKSPACE,
            ],
        )
        .await,
    );
    let agents = recorded(
        &client(
            &harness,
            &["agent", "list", "--organization", "acme", "--json", AGENT],
        )
        .await,
    );

    assert_eq!(workspaces, workspace);
    assert_eq!(
        workspaces[0]["repositories"],
        json!([
            "https://github.com/jtmthf/kestrel",
            "https://github.com/jtmthf/skills"
        ])
    );
    assert_eq!(workspaces[0]["branch"], "main");
    assert_eq!(agents, agent);
    assert_eq!(agents[0]["runtime"], "opencode");
    assert_eq!(agents[0]["model"], "claude-opus-5");

    let opened = harness.open_session("acme", "kestrel", "builder").await;
    assert_eq!(
        opened.workspace.id.to_string(),
        workspace[0]["id"].as_str().expect("an id")
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_operates_sessions_and_runs_without_opening_a_database() {
    let harness = Harness::boot().await;
    succeeded(&client(&harness, &["organization", "declare", "acme"]).await);
    succeeded(
        &client(
            &harness,
            &[
                "workspace",
                "declare",
                "kestrel",
                "--organization",
                "acme",
                "--repository",
                "https://github.com/jtmthf/kestrel",
                "--branch",
                "main",
            ],
        )
        .await,
    );
    succeeded(
        &client(
            &harness,
            &["agent", "declare", "builder", "--organization", "acme"],
        )
        .await,
    );

    let opened = recorded(
        &client(
            &harness,
            &[
                "session",
                "open",
                "--organization",
                "acme",
                "--workspace",
                "kestrel",
                "--agent",
                "builder",
                "--json",
                SESSION,
            ],
        )
        .await,
    );
    let session = opened[0]["id"].as_str().expect("a session id").to_owned();
    let session_name = generated_name(&opened[0]).to_owned();

    let listed = recorded(
        &client(
            &harness,
            &[
                "session",
                "list",
                "--organization",
                "acme",
                "--json",
                SESSION,
            ],
        )
        .await,
    );
    assert_eq!(listed, opened);
    let shown =
        recorded(&client(&harness, &["session", "show", &session, "--json", SESSION]).await);
    assert_eq!(shown, opened);
    assert_eq!(shown[0]["name"], session_name);

    let posted = recorded(
        &client(
            &harness,
            &[
                "session",
                "post",
                &session,
                "start with the operator boundary",
                "--json",
                RUN,
            ],
        )
        .await,
    );
    let run = posted[0]["id"].as_str().expect("a run id");
    let first_run_name = generated_name(&posted[0]).to_owned();
    assert_eq!(posted[0]["session"], session);
    assert_eq!(posted[0]["state"], "queued");
    assert_eq!(
        recorded(
            &client(
                &harness,
                &["run", "list", "--session", &session, "--json", RUN]
            )
            .await
        ),
        posted
    );

    let completed = harness
        .claim_run()
        .await
        .expect("the posted run should wait for the worker");
    assert_eq!(completed.run.id.to_string(), run);
    harness.complete_run(&completed.run).await;
    let sealed =
        recorded(&client(&harness, &["session", "seal", &session, "--json", SESSION]).await);
    assert_eq!(sealed[0]["state"], "sealed");

    let continued = recorded(
        &client(
            &harness,
            &[
                "session",
                "open",
                "--organization",
                "acme",
                "--workspace",
                "kestrel",
                "--agent",
                "builder",
                "--continues",
                &session,
                "--json",
                SESSION,
            ],
        )
        .await,
    );
    let continuing = continued[0]["id"].as_str().expect("a continuing session");
    assert_ne!(generated_name(&continued[0]), session_name);
    assert_eq!(continued[0]["continues"], session);
    let enqueued = recorded(
        &client(
            &harness,
            &[
                "run",
                "enqueue",
                "--session",
                continuing,
                "--model",
                "claude-opus-5",
                "--json",
                RUN,
            ],
        )
        .await,
    );
    assert_ne!(generated_name(&enqueued[0]), first_run_name);
    assert_eq!(enqueued[0]["session"], continuing);
    assert_eq!(enqueued[0]["model"], "claude-opus-5");
    assert_eq!(
        recorded(
            &client(
                &harness,
                &["run", "list", "--session", continuing, "--json", RUN]
            )
            .await
        ),
        enqueued
    );

    harness.teardown().await;
}

#[tokio::test]
async fn session_and_run_names_remain_unique_when_creation_retries_collisions() {
    let harness = Harness::boot().await;
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

    let mut session_names = HashSet::new();
    let mut run_names = HashSet::new();
    for _ in 0..100 {
        let session = harness.open_session("acme", "kestrel", "builder").await;
        assert!(session_names.insert(session.name));

        let run = harness.enqueue_run(session.id).await;
        assert!(run_names.insert(run.name));
    }

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_manages_triggers_without_opening_a_database() {
    let harness = Harness::boot().await;
    succeeded(&client(&harness, &["organization", "declare", "acme"]).await);
    succeeded(
        &client(
            &harness,
            &[
                "workspace",
                "declare",
                "kestrel",
                "--organization",
                "acme",
                "--repository",
                "https://github.com/jtmthf/kestrel",
                "--branch",
                "main",
            ],
        )
        .await,
    );
    succeeded(
        &client(
            &harness,
            &["agent", "declare", "builder", "--organization", "acme"],
        )
        .await,
    );
    let webhook = harness
        .register_webhook("acme", "events", "a-shared-secret")
        .await;
    let response = reqwest::Client::new()
        .post(format!("{}{}", harness.link(), webhook.webhook_path()))
        .bearer_auth("a-shared-secret")
        .header("content-type", "application/cloudevents+json")
        .body(
            json!({
                "id": "retained",
                "source": "urn:test",
                "specversion": "1.0",
                "type": "example",
                "time": "2026-09-19T00:00:00Z",
            })
            .to_string(),
        )
        .send()
        .await
        .expect("the webhook should answer");
    assert!(response.status().is_success());
    let retained = harness.events("acme").await[0].record_id.to_string();

    let declaration = [
        "trigger",
        "declare",
        "ready",
        "--organization",
        "acme",
        "--filter",
        r#"{"exact":{"type":"example"}}"#,
        "--brief",
        "Work {{ event.type }}",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
        "--json",
        TRIGGER,
    ];
    let declared = recorded(&client(&harness, &declaration).await);
    let trigger = declared[0]["id"].as_str().expect("a trigger id").to_owned();
    assert_eq!(declared[0]["name"], "ready");
    assert_eq!(declared[0]["state"], "enabled");

    let listed = recorded(
        &client(
            &harness,
            &[
                "trigger",
                "list",
                "--organization",
                "acme",
                "--json",
                TRIGGER,
            ],
        )
        .await,
    );
    assert_eq!(listed, declared);
    assert_eq!(
        recorded(
            &client(
                &harness,
                &[
                    "trigger",
                    "show",
                    "ready",
                    "--organization",
                    "acme",
                    "--json",
                    TRIGGER
                ]
            )
            .await
        ),
        declared
    );
    assert_eq!(
        recorded(&client(&harness, &declaration).await)[0]["id"],
        trigger
    );

    let changed = recorded(
        &client(
            &harness,
            &[
                "trigger",
                "declare",
                "ready",
                "--organization",
                "acme",
                "--filter",
                r#"{"exact":{"type":"example"}}"#,
                "--brief",
                "Triage {{ event.type }}",
                "--workspace",
                "kestrel",
                "--agent",
                "builder",
                "--json",
                TRIGGER,
            ],
        )
        .await,
    );
    assert_eq!(changed[0]["id"], trigger);
    assert_eq!(changed[0]["brief"], "Triage {{ event.type }}");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(harness.sessions("acme").await.is_empty());

    let test = vec![
        "trigger",
        "test",
        "ready",
        "--organization",
        "acme",
        "--event",
        &retained,
        "--json",
        "matches",
    ];
    let tested = recorded(&client(&harness, &test).await);
    assert_eq!(tested[0]["matches"], true);

    let disabled = recorded(
        &client(
            &harness,
            &[
                "trigger",
                "disable",
                "ready",
                "--organization",
                "acme",
                "--json",
                "state",
            ],
        )
        .await,
    );
    assert_eq!(disabled[0]["state"], "disabled:operator");
    let enabled = recorded(
        &client(
            &harness,
            &[
                "trigger",
                "enable",
                "ready",
                "--organization",
                "acme",
                "--json",
                "state",
            ],
        )
        .await,
    );
    assert_eq!(enabled[0]["state"], "enabled");

    harness.teardown().await;
}

#[tokio::test]
async fn the_operator_documents_trigger_answers_and_refusals() {
    let harness = Harness::boot().await;
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
    let triggers = triggers_of("acme");
    let declaration = json!({
        "name": "sweep",
        "every": "1h",
        "brief": "Sweep {{ event.data.trigger }}",
        "workspace": "kestrel",
        "agent": "builder",
    });

    assert!(listed_nothing(&harness, &triggers).await);
    let (status, trigger) = declared(&harness, &triggers, &declaration).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, repeated) = declared(&harness, &triggers, &declaration).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated["id"], trigger["id"]);
    let path = trigger_at("acme", "sweep");
    let (status, _) = got(&harness, &path).await;
    assert_eq!(status, StatusCode::OK);
    let (status, tested) = declared(&harness, &format!("{path}/test"), &json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tested["matches"], true);
    let (status, _) = declared(&harness, &format!("{path}/disable"), &json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = declared(&harness, &format!("{path}/enable"), &json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = declared(
        &harness,
        &triggers,
        &json!({
            "name": "broken",
            "filter": { "exact": { "type": "x" } },
            "every": "1h",
            "brief": "x",
            "workspace": "kestrel",
            "agent": "builder",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    harness.teardown().await;
}

#[tokio::test]
async fn the_operator_documents_session_and_run_answers_and_refusals() {
    let harness = Harness::boot().await;
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
    let sessions = operator::SESSIONS.replace("{organization}", "acme");

    let (status, _) = got(&harness, &sessions).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = declared(
        &harness,
        &sessions,
        &json!({ "workspace": "nowhere", "agent": "builder" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, opened) = declared(
        &harness,
        &sessions,
        &json!({ "workspace": "kestrel", "agent": "builder" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let session = opened["id"].as_str().expect("a session id");
    let shown = operator::SESSION.replace("{session}", session);
    let messages = operator::SESSION_MESSAGES.replace("{session}", session);
    let runs = operator::RUNS.replace("{session}", session);

    let (status, _) = got(&harness, &shown).await;
    assert_eq!(status, StatusCode::OK);
    let (status, posted) = declared(
        &harness,
        &messages,
        &json!({ "message": "start with the operator boundary" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(posted["session"], session);
    let (status, _) = got(&harness, &runs).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = declared(&harness, &runs, &json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let run = harness
        .claim_run()
        .await
        .expect("the posted run should wait for the worker");
    harness.complete_run(&run.run).await;
    let seal = operator::SESSION_SEAL.replace("{session}", session);
    let (status, _) = declared(&harness, &seal, &json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = declared(&harness, &seal, &json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = declared(
        &harness,
        &sessions,
        &json!({ "workspace": "kestrel", "agent": "builder", "continues": session }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let nowhere = operator::SESSION.replace("{session}", "01a0a2d8-baf8-7c02-99fa-7280f174c14a");
    let (status, _) = got(&harness, &nowhere).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    harness.teardown().await;
}

#[tokio::test]
async fn an_unchanged_declaration_repeated_answers_the_record_it_made() {
    let harness = Harness::boot().await;
    let workspace = json!({
        "name": "kestrel",
        "repositories": ["https://github.com/jtmthf/kestrel"],
        "branch": "main",
    });
    let agent = json!({ "name": "builder", "runtime": "opencode", "model": "claude-opus-5" });

    let declarations = [
        (
            operator::ORGANIZATIONS.to_owned(),
            json!({ "name": "acme" }),
        ),
        (workspaces_of("acme"), workspace),
        (agents_of("acme"), agent),
    ];

    for (path, declaration) in &declarations {
        let (created, first) = declared(&harness, path, declaration).await;
        let (repeated, second) = declared(&harness, path, declaration).await;

        assert_eq!(created, StatusCode::CREATED, "{first}");
        assert_eq!(repeated, StatusCode::OK, "{second}");
        assert_eq!(first, second);
    }
    assert_eq!(listed(&harness, operator::ORGANIZATIONS).await.len(), 1);
    assert_eq!(listed(&harness, &workspaces_of("acme")).await.len(), 1);
    assert_eq!(listed(&harness, &agents_of("acme")).await.len(), 1);

    harness.teardown().await;
}

#[tokio::test]
async fn a_changed_workspace_declaration_converges_on_the_workspace_by_that_name() {
    let harness = Harness::boot().await;
    declared(
        &harness,
        operator::ORGANIZATIONS,
        &json!({ "name": "acme" }),
    )
    .await;
    let (_, first) = declared(
        &harness,
        &workspaces_of("acme"),
        &json!({
            "name": "kestrel",
            "repositories": [
                "https://github.com/jtmthf/kestrel",
                "https://github.com/jtmthf/skills",
            ],
            "branch": "main",
        }),
    )
    .await;

    let (status, changed) = declared(
        &harness,
        &workspaces_of("acme"),
        &json!({
            "name": "kestrel",
            "repositories": ["https://github.com/jtmthf/skills"],
            "branch": "next",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["id"], first["id"]);
    assert_eq!(
        changed["repositories"],
        json!(["https://github.com/jtmthf/skills"])
    );
    assert_eq!(changed["branch"], "next");
    assert_eq!(
        listed(&harness, &workspaces_of("acme")).await,
        vec![changed]
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_changed_agent_declaration_converges_on_the_agent_by_that_name() {
    let harness = Harness::boot().await;
    declared(
        &harness,
        operator::ORGANIZATIONS,
        &json!({ "name": "acme" }),
    )
    .await;
    let (_, first) = declared(
        &harness,
        &agents_of("acme"),
        &json!({ "name": "builder", "runtime": "opencode", "model": "claude-opus-5" }),
    )
    .await;

    let (_, changed) = declared(
        &harness,
        &agents_of("acme"),
        &json!({ "name": "builder", "runtime": "claude-code", "model": "claude-sonnet-5" }),
    )
    .await;
    let (status, unnamed) = declared(
        &harness,
        &agents_of("acme"),
        &json!({ "name": "builder", "runtime": "claude-code" }),
    )
    .await;

    assert_eq!(changed["id"], first["id"]);
    assert_eq!(changed["runtime"], "claude-code");
    assert_eq!(changed["model"], "claude-sonnet-5");
    assert_eq!(status, StatusCode::OK, "{unnamed}");
    assert_eq!(unnamed["id"], first["id"]);
    assert_eq!(unnamed["model"], Value::Null);
    assert_eq!(listed(&harness, &agents_of("acme")).await, vec![unnamed]);

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_declaring_into_no_such_organization_is_refused() {
    let harness = Harness::boot().await;

    let refused = client(
        &harness,
        &[
            "workspace",
            "declare",
            "kestrel",
            "--organization",
            "acme",
            "--repository",
            "https://github.com/jtmthf/kestrel",
            "--branch",
            "main",
        ],
    )
    .await;
    let (status, refusal) = declared(
        &harness,
        &agents_of("acme"),
        &json!({ "name": "builder", "runtime": "opencode" }),
    )
    .await;

    assert!(!refused.status.success());
    assert!(
        refused.err.contains("no organization named acme"),
        "{}",
        refused.err
    );
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(refusal["message"], "no organization named acme");

    harness.teardown().await;
}

#[tokio::test]
async fn a_declaration_that_describes_nothing_declarable_is_refused() {
    let harness = Harness::boot().await;
    declared(
        &harness,
        operator::ORGANIZATIONS,
        &json!({ "name": "acme" }),
    )
    .await;

    let (malformed, _) = declared(
        &harness,
        operator::ORGANIZATIONS,
        &json!({ "title": "acme" }),
    )
    .await;
    let (unnamed, _) = declared(&harness, operator::ORGANIZATIONS, &json!({ "name": "" })).await;
    let (nowhere, refusal) = declared(
        &harness,
        &workspaces_of("acme"),
        &json!({ "name": "kestrel", "repositories": [], "branch": "main" }),
    )
    .await;
    let (clashing, clash) = declared(
        &harness,
        &workspaces_of("acme"),
        &json!({
            "name": "kestrel",
            "repositories": ["https://github.com/acme/api.git", "https://github.com/team/api"],
            "branch": "main",
        }),
    )
    .await;

    assert_eq!(malformed, StatusCode::BAD_REQUEST);
    assert_eq!(unnamed, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(nowhere, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refusal["message"]
            .as_str()
            .expect("a message")
            .contains("repository"),
        "{refusal}"
    );
    assert_eq!(clashing, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        clash["message"]
            .as_str()
            .expect("a message")
            .contains("checked out into api"),
        "{clash}"
    );
    assert!(listed(&harness, &workspaces_of("acme")).await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn an_agent_naming_a_model_its_runtime_does_not_advertise_is_refused() {
    let harness = Harness::boot().await;
    let organization = harness.declare_organization("acme").await;
    harness
        .advertised(&organization, "opencode", &["claude-opus-5"])
        .await;

    let (status, refusal) = declared(
        &harness,
        &agents_of("acme"),
        &json!({ "name": "builder", "runtime": "opencode", "model": "gpt-9" }),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refusal["message"]
            .as_str()
            .expect("a message")
            .contains("claude-opus-5"),
        "the refusal does not say what the runtime offers: {refusal}"
    );
    assert!(listed(&harness, &agents_of("acme")).await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_sets_lists_and_forgets_provider_credentials_without_saying_them() {
    let harness = Harness::boot().await;
    let organization = harness.declare_organization("acme").await;
    let secret = "sk-kestrel-should-never-say-this";

    let set = client_given(
        &harness,
        &[
            "credential",
            "set",
            "ANTHROPIC_API_KEY",
            "--organization",
            "acme",
            "--json",
            CREDENTIAL,
        ],
        Some(&format!("{secret}\n")),
    )
    .await;
    let held = recorded(&set);
    let listed = client(
        &harness,
        &[
            "credential",
            "list",
            "--organization",
            "acme",
            "--json",
            CREDENTIAL,
        ],
    )
    .await;

    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["variable"], "ANTHROPIC_API_KEY");
    assert_eq!(recorded(&listed), held);
    for said in [&set, &listed] {
        assert!(
            !said.out.join("\n").contains(secret) && !said.err.contains(secret),
            "the client spelled the credential out"
        );
    }
    assert_eq!(
        harness.provider_credentials_held(&organization).await[0].variable,
        "ANTHROPIC_API_KEY"
    );

    let forgotten = client(
        &harness,
        &[
            "credential",
            "forget",
            "ANTHROPIC_API_KEY",
            "--organization",
            "acme",
        ],
    )
    .await;
    assert!(succeeded(&forgotten).is_empty());
    assert!(listed_nothing(&harness, &credentials_of("acme")).await);
    assert!(
        harness
            .provider_credentials_held(&organization)
            .await
            .is_empty()
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_credential_answers_back_what_it_is_read_from_and_never_its_value() {
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;

    let (status, held) = requested(
        &harness,
        reqwest::Method::PUT,
        &credential_of("acme", "OPENAI_API_KEY"),
        Some(&json!({ "secret": "the-first-key" })),
    )
    .await;
    let (replaced, again) = requested(
        &harness,
        reqwest::Method::PUT,
        &credential_of("acme", "OPENAI_API_KEY"),
        Some(&json!({ "secret": "the-second-key" })),
    )
    .await;
    let listed = listed(&harness, &credentials_of("acme")).await;

    assert_eq!(status, StatusCode::OK, "{held}");
    assert_eq!(replaced, StatusCode::OK, "{again}");
    assert_eq!(listed, vec![again.clone()]);
    for answered in [&held, &again, &Value::Array(listed)] {
        let answered = answered.to_string();
        assert!(
            !answered.contains("the-first-key") && !answered.contains("the-second-key"),
            "the boundary answered a secret: {answered}"
        );
    }

    harness.teardown().await;
}

#[tokio::test]
async fn a_credential_no_process_could_carry_or_nobody_holds_is_refused() {
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;

    let (unnamed, refusal) = requested(
        &harness,
        reqwest::Method::PUT,
        &credential_of("acme", "NOT-A-VARIABLE"),
        Some(&json!({ "secret": "a-key" })),
    )
    .await;
    let (empty, _) = requested(
        &harness,
        reqwest::Method::PUT,
        &credential_of("acme", "A_KEY"),
        Some(&json!({ "secret": "" })),
    )
    .await;
    let (nowhere, _) = requested(
        &harness,
        reqwest::Method::PUT,
        &credential_of("globex", "A_KEY"),
        Some(&json!({ "secret": "a-key" })),
    )
    .await;
    let (unheld, _) = requested(
        &harness,
        reqwest::Method::DELETE,
        &credential_of("acme", "A_KEY"),
        None,
    )
    .await;
    let nothing_on_stdin = client_given(
        &harness,
        &["credential", "set", "A_KEY", "--organization", "acme"],
        Some(""),
    )
    .await;
    let forgetting = client(
        &harness,
        &["credential", "forget", "A_KEY", "--organization", "acme"],
    )
    .await;

    assert_eq!(unnamed, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refusal["message"]
            .as_str()
            .expect("a message")
            .contains("NOT-A-VARIABLE"),
        "{refusal}"
    );
    assert_eq!(empty, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(nowhere, StatusCode::NOT_FOUND);
    assert_eq!(unheld, StatusCode::NOT_FOUND);
    assert!(failed(&nothing_on_stdin).contains("standard input"));
    assert!(failed(&forgetting).contains("holds no provider credential named A_KEY"));
    assert!(listed_nothing(&harness, &credentials_of("acme")).await);

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_registers_and_lists_integrations_without_saying_their_secrets() {
    let harness = Harness::boot().await;
    let stub = GithubStub::start();
    harness.declare_organization("acme").await;

    let github = client(
        &harness,
        &[
            "integration",
            "register",
            "github",
            "hub",
            "--organization",
            "acme",
            "--repository",
            "jtmthf/kestrel",
            "--token",
            TOKEN,
            "--api",
            &stub.base_url(),
            "--interval",
            "5m",
            "--json",
            INTEGRATION,
        ],
    )
    .await;
    let webhook = client(
        &harness,
        &[
            "integration",
            "register",
            "webhook",
            "ci",
            "--organization",
            "acme",
            "--secret",
            "a-shared-secret",
            "--json",
            INTEGRATION,
        ],
    )
    .await;
    let listed = client(
        &harness,
        &[
            "integration",
            "list",
            "--organization",
            "acme",
            "--json",
            INTEGRATION,
        ],
    )
    .await;

    let github = recorded(&github);
    let webhook = recorded(&webhook);
    let records = recorded(&listed);
    assert_eq!(records, [webhook.clone(), github.clone()].concat());
    assert_eq!(github[0]["kind"], "github");
    assert_eq!(github[0]["repository"], "jtmthf/kestrel");
    assert_eq!(github[0]["carries"], json!(["inbound", "outbound"]));
    assert_eq!(github[0]["polled_every"], "5m");
    assert_eq!(github[0]["webhook_path"], Value::Null);
    assert_eq!(webhook[0]["kind"], "webhook");
    assert_eq!(webhook[0]["carries"], json!(["inbound"]));
    assert_eq!(
        webhook[0]["webhook_path"],
        format!("/webhooks/{}", webhook[0]["id"].as_str().expect("an id"))
    );
    assert_eq!(webhook[0]["last_event_refusal"], Value::Null);
    let said = listed.out.join("\n");
    assert!(!said.contains(TOKEN), "the listing spelled the token out");
    assert!(
        !said.contains("a-shared-secret"),
        "the listing spelled the webhook secret out"
    );
    assert_eq!(
        harness.integrations("acme").await[0].id.to_string(),
        webhook[0]["id"].as_str().expect("an id")
    );

    harness.teardown().await;
}

#[tokio::test]
async fn an_integration_is_registered_with_what_it_is_declared_to_carry() {
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;

    let (status, signed) = declared(
        &harness,
        &integrations_of("acme"),
        &json!({
            "kind": "github",
            "name": "hub",
            "repository": "jtmthf/kestrel",
            "token": TOKEN,
            "carries": ["outbound"],
            "webhook_secret": "a-signing-secret",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{signed}");
    assert_eq!(signed["carries"], json!(["outbound"]));
    assert_eq!(signed["polled_every"], Value::Null);
    assert!(signed["webhook_path"].is_string(), "{signed}");
    assert!(!signed.to_string().contains("a-signing-secret"));
    assert!(!signed.to_string().contains(TOKEN));

    harness.teardown().await;
}

#[tokio::test]
async fn a_registration_that_describes_no_usable_integration_is_refused() {
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;
    let webhook = json!({ "kind": "webhook", "name": "ci", "secret": "a-shared-secret" });
    declared(&harness, &integrations_of("acme"), &webhook).await;

    let refusals = [
        (
            integrations_of("acme"),
            webhook.clone(),
            StatusCode::CONFLICT,
        ),
        (
            integrations_of("globex"),
            webhook.clone(),
            StatusCode::NOT_FOUND,
        ),
        (
            integrations_of("acme"),
            json!({ "kind": "pager", "name": "pd" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            integrations_of("acme"),
            json!({ "kind": "webhook", "name": "", "secret": "a-shared-secret" }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            integrations_of("acme"),
            json!({ "kind": "webhook", "name": "out", "secret": "s", "carries": ["outbound"] }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            integrations_of("acme"),
            json!({ "kind": "github", "name": "hub", "repository": "kestrel", "token": TOKEN }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            integrations_of("acme"),
            json!({
                "kind": "github",
                "name": "hub",
                "repository": "jtmthf/kestrel",
                "token": TOKEN,
                "interval": "whenever",
            }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            integrations_of("acme"),
            json!({
                "kind": "github",
                "name": "hub",
                "repository": "jtmthf/kestrel",
                "token": TOKEN,
                "carries": [],
            }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ];
    for (path, registration, expected) in &refusals {
        let (status, refusal) = declared(&harness, path, registration).await;
        assert_eq!(status, *expected, "{registration} was answered {refusal}");
        assert!(
            !refusal.to_string().contains(TOKEN),
            "a refusal spelled the token out: {refusal}"
        );
    }
    let taken = client(
        &harness,
        &[
            "integration",
            "register",
            "webhook",
            "ci",
            "--organization",
            "acme",
            "--secret",
            "another",
        ],
    )
    .await;

    assert!(failed(&taken).contains("already has an integration named ci"));
    assert_eq!(listed(&harness, &integrations_of("acme")).await.len(), 1);

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_acknowledges_the_event_an_integration_refused() {
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;
    let webhook = harness
        .register_webhook("acme", "ci", "a-shared-secret")
        .await;
    reqwest::Client::new()
        .post(format!("{}{}", harness.link(), webhook.webhook_path()))
        .bearer_auth("a-shared-secret")
        .header("content-type", "text/plain")
        .body("x".repeat(1024 * 1024 + 1))
        .send()
        .await
        .expect("the webhook answers");

    let refused = recorded(
        &client(
            &harness,
            &[
                "integration",
                "list",
                "--organization",
                "acme",
                "--json",
                "last_event_refusal",
            ],
        )
        .await,
    );
    let acknowledged = client(
        &harness,
        &[
            "integration",
            "acknowledge-refusal",
            "ci",
            "--organization",
            "acme",
        ],
    )
    .await;
    let (status, _) = requested(
        &harness,
        reqwest::Method::DELETE,
        &event_refusal_of("acme", "ci"),
        None,
    )
    .await;
    let (unknown, _) = requested(
        &harness,
        reqwest::Method::DELETE,
        &event_refusal_of("acme", "pager"),
        None,
    )
    .await;

    let refusal = &refused[0]["last_event_refusal"];
    assert!(
        refusal["bytes"].as_u64().expect("a size") > 1024 * 1024,
        "{refusal}"
    );
    assert!(refusal["reason"].is_string(), "{refusal}");
    assert!(succeeded(&acknowledged).is_empty());
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(unknown, StatusCode::NOT_FOUND);
    assert_eq!(
        listed(&harness, &integrations_of("acme")).await[0]["last_event_refusal"],
        Value::Null
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_lists_an_organizations_events_and_shows_one_whole() {
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;
    let webhook = harness
        .register_webhook("acme", "ci", "a-shared-secret")
        .await;
    for id in ["deploy-1", "deploy-2"] {
        let answered = reqwest::Client::new()
            .post(format!("{}{}", harness.link(), webhook.webhook_path()))
            .bearer_auth("a-shared-secret")
            .header("content-type", "application/cloudevents+json")
            .body(
                json!({
                    "specversion": "1.0",
                    "id": id,
                    "source": "/argo/sensors/deploy",
                    "type": "io.argoproj.deployed",
                    "subject": "kestrel",
                    "data": { "image": "kestrel:1" },
                })
                .to_string(),
            )
            .send()
            .await
            .expect("the webhook answers");
        assert_eq!(answered.status(), StatusCode::ACCEPTED);
    }

    let events = recorded(
        &client(
            &harness,
            &["event", "list", "--organization", "acme", "--json", EVENT],
        )
        .await,
    );
    let limited = recorded(
        &client(
            &harness,
            &[
                "event",
                "list",
                "--organization",
                "acme",
                "--limit",
                "1",
                "--json",
                EVENT,
            ],
        )
        .await,
    );
    let record = events[0]["record"].as_str().expect("a record id");
    let shown = recorded(&client(&harness, &["event", "show", record, "--json", EVENT]).await);

    assert_eq!(events.len(), 2);
    assert_eq!(limited, events[..1]);
    assert_eq!(shown, events[..1]);
    assert_eq!(shown[0]["integration"], webhook.id.to_string());
    assert_eq!(shown[0]["event"]["source"], "/argo/sensors/deploy");
    assert_eq!(shown[0]["event"]["type"], "io.argoproj.deployed");
    assert_eq!(shown[0]["event"]["specversion"], "1.0");
    assert_eq!(shown[0]["event"]["subject"], "kestrel");
    assert_eq!(shown[0]["event"]["data"], json!({ "image": "kestrel:1" }));
    assert_eq!(
        harness.events("acme").await[0].record_id.to_string(),
        record
    );
    let (_, over_the_boundary) = got(&harness, &format!("{}?limit=1", events_of("acme"))).await;
    assert_eq!(over_the_boundary.as_array().map(Vec::len), Some(1));
    assert_eq!(over_the_boundary[0]["record"], limited[0]["record"]);

    harness.teardown().await;
}

#[tokio::test]
async fn an_event_nobody_recorded_is_refused() {
    let harness = Harness::boot().await;

    let (unrecorded, refusal) =
        got(&harness, &event_at(&EventRecordId::generate().to_string())).await;
    let (malformed, _) = got(&harness, &event_at("yesterday")).await;
    let (nowhere, _) = got(&harness, &events_of("acme")).await;
    let showing = client(&harness, &["event", "show", "yesterday"]).await;

    assert_eq!(unrecorded, StatusCode::NOT_FOUND);
    assert!(
        refusal["message"]
            .as_str()
            .expect("a message")
            .starts_with("no event"),
        "{refusal}"
    );
    assert_eq!(malformed, StatusCode::NOT_FOUND);
    assert_eq!(nowhere, StatusCode::NOT_FOUND);
    assert!(failed(&showing).contains("no event yesterday"));

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_in_its_own_process_reads_a_transcript_over_the_operator_boundary() {
    let harness = Harness::boot().await;
    let (session, _) = an_open_session(&harness, 2).await;
    let (operator, reading) = (harness.operator(), session.clone());

    let read = tokio::task::spawn_blocking(move || {
        client::ran(
            &operator,
            &["session", "transcript", &reading, "--json", ENTRY],
        )
    })
    .await
    .expect("the client should run");

    assert!(read.status.success(), "the client failed:\n{}", read.err);
    assert_eq!(seqs(&read.out), recorded_seqs(&harness, &session).await);
    let said: Value = serde_json::from_str(&read.out[read.out.len() - 1]).expect("an entry");
    assert_eq!(said["entry"]["kind"], "said");
    assert_eq!(said["entry"]["message"], "message 2");
    assert!(
        read.err.contains("cursor  "),
        "the client printed no cursor to resume from:\n{}",
        read.err
    );
    assert!(
        read.left_behind.is_empty(),
        "the client wrote {:?} where it ran",
        read.left_behind
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_handed_a_cursor_reads_only_what_came_after_it() {
    let harness = Harness::boot().await;
    let (session, run) = an_open_session(&harness, 1).await;
    let operator = harness.operator();

    let first = {
        let (operator, session) = (operator.clone(), session.clone());
        tokio::task::spawn_blocking(move || {
            client::ran(&operator, &["session", "transcript", &session])
        })
        .await
        .expect("the client should run")
    };
    let cursor = first
        .err
        .lines()
        .find_map(|line| line.strip_prefix("cursor  "))
        .expect("a cursor")
        .to_owned();
    harness.said(&run, "said after the first read").await;

    let second = tokio::task::spawn_blocking(move || {
        client::ran(
            &operator,
            &["session", "transcript", &session, "--cursor", &cursor],
        )
    })
    .await
    .expect("the client should run");

    assert!(
        second.status.success(),
        "the client failed:\n{}",
        second.err
    );
    assert_eq!(second.out.len(), 1, "read {:?}", second.out);
    assert!(second.out[0].contains("said after the first read"));

    harness.teardown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_following_client_resumes_across_a_restart_without_repeating_an_entry() {
    let harness = Harness::boot().await;
    let (session, run) = an_open_session(&harness, 2).await;
    let before = recorded_seqs(&harness, &session).await.len();

    let mut client = Client::spawn(
        &harness.operator(),
        &[
            "session",
            "transcript",
            &session,
            "--follow",
            "--json",
            ENTRY,
        ],
    );
    let mut read = Vec::new();
    for _ in 0..before {
        read.push(tokio::task::block_in_place(|| client.line()));
    }

    harness.said(&run, "said while it followed").await;
    read.push(tokio::task::block_in_place(|| client.line()));
    harness.complete_run(&run).await;

    let harness = harness.teardown().await.restart().await;
    harness.said(&run, "said after the restart").await;
    harness
        .seal_session(session.parse().expect("a session id"))
        .await;

    let finished = tokio::task::block_in_place(|| client.finish());
    read.extend(finished.out);

    assert!(
        finished.status.success(),
        "the client failed:\n{}",
        finished.err
    );
    assert_eq!(seqs(&read), recorded_seqs(&harness, &session).await);
    assert!(
        read.last()
            .expect("an entry")
            .contains("said after the restart")
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_client_asking_for_no_such_session_is_refused() {
    let harness = Harness::boot().await;
    let operator = harness.operator();

    let read = tokio::task::spawn_blocking(move || {
        client::ran(
            &operator,
            &[
                "session",
                "transcript",
                "01a0a2d8-baf8-7c02-99fa-7280f174c14a",
            ],
        )
    })
    .await
    .expect("the client should run");

    assert!(!read.status.success());
    assert!(read.err.contains("no such session"), "{}", read.err);

    harness.teardown().await;
}

#[tokio::test]
async fn a_cursor_from_another_transcript_is_refused_rather_than_restarting_the_walk() {
    let harness = Harness::boot().await;
    let (session, _) = an_open_session(&harness, 1).await;
    let elsewhere = format!("{}:1", RunId::generate());

    let response = reqwest::Client::new()
        .get(format!(
            "{}{}",
            harness.operator(),
            operator::TRANSCRIPT.replace("{session}", &session)
        ))
        .header("last-event-id", elsewhere)
        .send()
        .await
        .expect("the operator boundary should answer");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    harness.teardown().await;
}

#[tokio::test]
async fn the_operator_boundary_and_the_link_are_served_apart() {
    let harness = Harness::boot().await;
    let (session, run) = an_open_session(&harness, 0).await;
    let client = reqwest::Client::new();

    let link_on_the_operator_listener = client
        .get(format!(
            "{}{}",
            harness.operator(),
            link::ENTRIES.replace("{run}", &run.id.to_string())
        ))
        .send()
        .await
        .expect("the operator listener should answer");
    let operator_on_the_link_listener = client
        .get(format!(
            "{}{}?follow=false",
            harness.link(),
            operator::TRANSCRIPT.replace("{session}", &session)
        ))
        .send()
        .await
        .expect("the link listener should answer");

    assert_ne!(harness.operator(), harness.link());
    assert_eq!(
        link_on_the_operator_listener.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        operator_on_the_link_listener.status(),
        StatusCode::NOT_FOUND
    );

    harness.teardown().await;
}

#[tokio::test]
async fn the_operator_boundary_asks_for_no_credential() {
    let harness = Harness::boot().await;
    let (session, _) = an_open_session(&harness, 0).await;

    let response = reqwest::Client::new()
        .get(format!(
            "{}{}?follow=false",
            harness.operator(),
            operator::TRANSCRIPT.replace("{session}", &session)
        ))
        .send()
        .await
        .expect("the operator boundary should answer");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .text()
            .await
            .expect("the stream should end")
            .contains("event: end")
    );

    harness.teardown().await;
}

#[test]
fn the_published_operator_document_describes_the_boundary_the_control_plane_serves() {
    let document = published();

    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(document["security"], serde_json::json!([]));

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

    let served = [
        (operator::ORGANIZATIONS, "get"),
        (operator::ORGANIZATIONS, "post"),
        (operator::WORKSPACES, "get"),
        (operator::WORKSPACES, "post"),
        (operator::AGENTS, "get"),
        (operator::AGENTS, "post"),
        (operator::CREDENTIALS, "get"),
        (operator::CREDENTIAL, "put"),
        (operator::CREDENTIAL, "delete"),
        (operator::PROFILES, "get"),
        (operator::PROFILES, "post"),
        (operator::PROFILE_VARIABLE, "put"),
        (operator::PROFILE_VARIABLE, "delete"),
        (operator::PROFILE_FILE, "put"),
        (operator::PROFILE_FILE, "delete"),
        (operator::INTEGRATIONS, "get"),
        (operator::INTEGRATIONS, "post"),
        (operator::EVENT_REFUSAL, "delete"),
        (operator::EVENTS, "get"),
        (operator::EVENT, "get"),
        (operator::SESSIONS, "get"),
        (operator::SESSIONS, "post"),
        (operator::SESSION, "get"),
        (operator::SESSION_MESSAGES, "post"),
        (operator::SESSION_SEAL, "post"),
        (operator::RUNS, "get"),
        (operator::RUNS, "post"),
        (operator::TRIGGERS, "get"),
        (operator::TRIGGERS, "post"),
        (operator::TRIGGER, "get"),
        (operator::TRIGGER_TEST, "post"),
        (operator::TRIGGER_DISABLE, "post"),
        (operator::TRIGGER_ENABLE, "post"),
        (operator::TRANSCRIPT, "get"),
    ];
    assert_eq!(
        described,
        served
            .iter()
            .map(|(path, method)| ((*path).to_owned(), (*method).to_owned()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_published_operator_document_describes_every_transcript_entry() {
    let document = published();
    let mapping = document["components"]["schemas"]["Entry"]["discriminator"]["mapping"]
        .as_object()
        .expect("an object of entry kinds");

    let served = [
        Entry::ParticipantJoined {
            participant: "builder".to_owned(),
        },
        Entry::Brief {
            trigger: "sweep".to_owned(),
            brief: "Sweep the backlog".to_owned(),
        },
        Entry::RunStarted {
            run: RunId::generate(),
        },
        Entry::Said {
            participant: "builder".to_owned(),
            message: "what the agent said".to_owned(),
        },
        Entry::Messages {
            messages: vec![Message {
                participant: "operator".to_owned(),
                message: "what arrived while it worked".to_owned(),
            }],
        },
        Entry::RunEnded {
            run: RunId::generate(),
            exit: Exit::Succeeded,
        },
        Entry::InstanceReleased {
            participant: "operator".to_owned(),
            instance: "docker/kestrel-01999cf2".to_owned(),
            unpublished: Some("https://github.com/acme/widgets has 1 untracked file".to_owned()),
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
                "the document requires {field} on a {kind} entry, and the boundary does not serve it"
            );
        }
        kinds.push(kind);
    }

    assert_eq!(kinds, mapping.keys().cloned().collect::<Vec<_>>());
}

fn published() -> Value {
    let document = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi/operator.json");

    serde_json::from_str(&fs::read_to_string(document).expect("a readable openapi document"))
        .expect("valid json")
}

/// The documented answer for this status must exist, and every field it requires must be served.
fn conforms(path: &str, method: &str, status: StatusCode, body: &Value) {
    let document = published();
    let (_, operations) = document["paths"]
        .as_object()
        .expect("an object of paths")
        .iter()
        .find(|(template, _)| matches_template(template, path))
        .unwrap_or_else(|| panic!("the document describes no path matching {path}"));
    let answer = &operations[method]["responses"][status.as_str()];
    assert!(
        !answer.is_null(),
        "the document says {method} {path} never answers {status}"
    );
    let answer = match answer["$ref"].as_str() {
        Some(reference) => resolve(&document, reference),
        None => answer,
    };

    let schema = &answer["content"]["application/json"]["schema"];
    if schema.is_null() {
        assert!(
            body.is_null(),
            "the document says {method} {path} answers {status} with nothing, and it served {body}"
        );
        return;
    }
    requires(&document, schema, body);
}

fn requires(document: &Value, schema: &Value, body: &Value) {
    let schema = match schema["$ref"].as_str() {
        Some(reference) => resolve(document, reference),
        None => schema,
    };
    if let Some(options) = schema["anyOf"].as_array() {
        let option = options
            .iter()
            .find(|option| (option["type"] == "null") == body.is_null())
            .expect("the documented alternatives include the answer");
        requires(document, option, body);
        return;
    }
    if schema["type"] == "array" {
        for item in body.as_array().expect("an array, as documented") {
            requires(document, &schema["items"], item);
        }
        return;
    }

    for field in schema["required"]
        .as_array()
        .expect("an array of required fields")
    {
        let field = field.as_str().expect("a named field");
        assert!(
            body.get(field).is_some(),
            "the document requires {field}, and the boundary served {body}"
        );
    }
}

fn matches_template(template: &str, path: &str) -> bool {
    let (template, path): (Vec<_>, Vec<_>) =
        (template.split('/').collect(), path.split('/').collect());
    template.len() == path.len()
        && template
            .iter()
            .zip(&path)
            .all(|(step, given)| step.starts_with('{') || step == given)
}

fn resolve<'a>(document: &'a Value, reference: &str) -> &'a Value {
    reference
        .trim_start_matches("#/")
        .split('/')
        .fold(document, |document, step| &document[step])
}
