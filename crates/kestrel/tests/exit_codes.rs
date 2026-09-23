mod support;

use std::net::TcpListener;

use serde_json::json;
use support::Harness;
use support::client::{Finished, Invocation, ran, ran_by};

const SUCCESS: i32 = 0;
const USAGE: i32 = 2;
const UNRESOLVED: i32 = 3;
const REJECTED: i32 = 4;
const UNAVAILABLE: i32 = 5;

fn exited(finished: &Finished, code: i32) {
    assert_eq!(
        finished.status.code(),
        Some(code),
        "stdout: {:?}\nstderr: {}",
        finished.out,
        finished.err
    );
}

/// A port something listened on a moment ago and nothing does now.
fn nowhere() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let address = listener.local_addr().expect("a bound address");
    drop(listener);

    format!("http://{address}")
}

async fn an_organization() -> Harness {
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;

    harness
}

#[test]
fn the_catalog_is_printed_without_reaching_a_control_plane() {
    let finished = ran(&nowhere(), &["exit-codes", "--json", "code,name"]);

    exited(&finished, SUCCESS);
    assert_eq!(
        finished.records(),
        [
            json!({ "code": 0, "name": "success" }),
            json!({ "code": 1, "name": "failure" }),
            json!({ "code": 2, "name": "usage" }),
            json!({ "code": 3, "name": "unresolved" }),
            json!({ "code": 4, "name": "rejected" }),
            json!({ "code": 5, "name": "unavailable" }),
        ]
    );
}

#[test]
fn every_entry_says_what_it_means_and_when_to_branch_on_it() {
    let finished = ran(&nowhere(), &["exit-codes", "--json", "code,meaning,branch"]);

    for entry in finished.records() {
        for field in ["meaning", "branch"] {
            assert!(
                entry[field].as_str().is_some_and(|text| !text.is_empty()),
                "{entry} says nothing for {field}"
            );
        }
    }
}

#[test]
fn help_points_at_the_catalog() {
    let finished = ran(&nowhere(), &["--help"]);

    exited(&finished, SUCCESS);
    assert!(
        finished
            .out
            .iter()
            .any(|line| line.contains("kestrel exit-codes")),
        "{:?}",
        finished.out
    );
}

#[test]
fn an_invalid_invocation_is_usage() {
    for args in [
        &["no-such-command"][..],
        &["organization", "list", "--no-such-flag"],
        &["organization", "list", "--organization", "acme"],
        &["organization", "list", "--json", " , "],
        &["--control-plane", "not a url", "organization", "list"],
        &["apply", "-f", "no-such-file.yaml"],
    ] {
        let finished = ran(&nowhere(), args);

        exited(&finished, USAGE);
    }
}

#[tokio::test]
async fn no_organization_in_scope_is_unresolved() {
    let harness = Harness::boot().await;

    let finished = ran_by(&harness, &["workspace", "list"], Invocation::default()).await;

    exited(&finished, UNRESOLVED);
    harness.teardown().await;
}

#[tokio::test]
async fn a_record_that_does_not_exist_is_unresolved() {
    let harness = an_organization().await;

    for args in [
        &["session", "show", "no-such-session"][..],
        &["trigger", "show", "no-such-trigger"],
        &["workspace", "list", "--organization", "globex"],
    ] {
        let finished = ran_by(&harness, args, Invocation::default()).await;

        exited(&finished, UNRESOLVED);
    }
    harness.teardown().await;
}

#[tokio::test]
async fn a_declined_operation_is_rejected() {
    let harness = an_organization().await;
    let declared = ran_by(
        &harness,
        &["profile", "declare", "max", "--owner", "max"],
        Invocation::default(),
    )
    .await;
    exited(&declared, SUCCESS);

    let taken = ran_by(
        &harness,
        &["profile", "declare", "max", "--owner", "someone-else"],
        Invocation::default(),
    )
    .await;
    let unacceptable = ran_by(
        &harness,
        &[
            "workspace",
            "declare",
            "kestrel",
            "--repository",
            "https://github.com/jtmthf/kestrel",
            "--branch",
            "",
        ],
        Invocation::default(),
    )
    .await;

    exited(&taken, REJECTED);
    exited(&unacceptable, REJECTED);
    harness.teardown().await;
}

#[test]
fn a_control_plane_nothing_answers_for_is_unavailable() {
    let finished = ran(&nowhere(), &["organization", "list"]);

    exited(&finished, UNAVAILABLE);
}
