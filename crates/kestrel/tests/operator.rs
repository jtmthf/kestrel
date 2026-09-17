mod support;

use std::fs;
use std::path::Path;

use kestrel::domain::{Exit, RunId};
use kestrel::link;
use kestrel::log::{Entry, Message};
use kestrel::operator;
use reqwest::StatusCode;
use serde_json::Value;
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

    assert_eq!(
        described,
        vec![(operator::TRANSCRIPT.to_owned(), "get".to_owned())]
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

fn resolve<'a>(document: &'a Value, reference: &str) -> &'a Value {
    reference
        .trim_start_matches("#/")
        .split('/')
        .fold(document, |document, step| &document[step])
}
