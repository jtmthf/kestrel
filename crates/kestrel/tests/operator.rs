mod support;

use std::fs;
use std::path::Path;

use kestrel::domain::{Exit, RunId};
use kestrel::link;
use kestrel::log::{Entry, Message};
use kestrel::operator;
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::Harness;
use support::client::{self, Client};

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

async fn client(harness: &Harness, args: &[&str]) -> client::Finished {
    let operator = harness.operator();
    let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();

    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        client::ran(&operator, &args)
    })
    .await
    .expect("the client should run")
}

fn succeeded(finished: &client::Finished) -> Vec<Value> {
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
    records(&finished.out)
}

/// Every answer is checked against what the published document says the operation answers.
async fn declared(harness: &Harness, path: &str, declaration: &Value) -> (StatusCode, Value) {
    let response = reqwest::Client::new()
        .post(format!("{}{path}", harness.operator()))
        .json(declaration)
        .send()
        .await
        .expect("the operator boundary should answer");
    let status = response.status();
    let body: Value = response.json().await.expect("a JSON answer");

    conforms(path, "post", status, &body);
    (status, body)
}

async fn listed(harness: &Harness, path: &str) -> Vec<Value> {
    let response = reqwest::Client::new()
        .get(format!("{}{path}", harness.operator()))
        .send()
        .await
        .expect("the operator boundary should answer");
    let status = response.status();
    let body: Value = response.json().await.expect("a JSON answer");

    conforms(path, "get", status, &body);
    assert_eq!(status, StatusCode::OK, "{body}");
    body.as_array().expect("an array of records").clone()
}

fn workspaces_of(organization: &str) -> String {
    operator::WORKSPACES.replace("{organization}", organization)
}

fn agents_of(organization: &str) -> String {
    operator::AGENTS.replace("{organization}", organization)
}

#[tokio::test]
async fn a_client_declares_and_lists_organizations_without_opening_a_database() {
    let harness = Harness::boot().await;

    let declared = succeeded(&client(&harness, &["organization", "declare", "acme"]).await);
    succeeded(&client(&harness, &["organization", "declare", "globex"]).await);
    let listed = succeeded(&client(&harness, &["organization", "list"]).await);

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

    let workspace = succeeded(
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
            ],
        )
        .await,
    );
    let agent = succeeded(
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
            ],
        )
        .await,
    );
    let workspaces =
        succeeded(&client(&harness, &["workspace", "list", "--organization", "acme"]).await);
    let agents = succeeded(&client(&harness, &["agent", "list", "--organization", "acme"]).await);

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
async fn a_client_in_its_own_process_reads_a_transcript_over_the_operator_boundary() {
    let harness = Harness::boot().await;
    let (session, _) = an_open_session(&harness, 2).await;
    let (operator, reading) = (harness.operator(), session.clone());

    let read = tokio::task::spawn_blocking(move || {
        client::ran(&operator, &["session", "transcript", &reading])
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
        &["session", "transcript", &session, "--follow"],
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

    requires(
        &document,
        &answer["content"]["application/json"]["schema"],
        body,
    );
}

fn requires(document: &Value, schema: &Value, body: &Value) {
    let schema = match schema["$ref"].as_str() {
        Some(reference) => resolve(document, reference),
        None => schema,
    };
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
