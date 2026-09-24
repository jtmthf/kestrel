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

#[test]
fn guessed_session_and_run_verbs_explain_the_domain_verbs_without_running_them() {
    for (args, suggested, verbs) in [
        (
            &["session", "create"][..],
            "session open",
            "open, list, show, post, seal, transcript",
        ),
        (
            &["session", "close"],
            "session seal",
            "open, list, show, post, seal, transcript",
        ),
        (
            &["run", "start"],
            "run enqueue",
            "enqueue, list, show, stop",
        ),
        (
            &["run", "enqueu"],
            "run enqueue",
            "enqueue, list, show, stop",
        ),
        (
            &["session", "opne"],
            "session open",
            "open, list, show, post, seal, transcript",
        ),
        (
            &["session", "sael"],
            "session seal",
            "open, list, show, post, seal, transcript",
        ),
    ] {
        let finished = ran(&nowhere(), args);
        exited(&finished, USAGE);
        assert!(finished.err.contains(suggested), "{}", finished.err);
        assert!(finished.err.contains(verbs), "{}", finished.err);
        assert!(finished.out.is_empty());
    }
}

#[tokio::test]
async fn no_organization_in_scope_is_unresolved() {
    let harness = Harness::boot().await;

    let finished = ran_by(&harness, &["workspace", "list"], Invocation::default()).await;

    exited(&finished, UNRESOLVED);
    assert!(
        finished
            .err
            .contains("kestrel organization declare default")
    );
    harness.teardown().await;
}

#[tokio::test]
async fn ambiguous_and_empty_organization_scope_offer_runnable_choices() {
    let harness = an_organization().await;
    harness.declare_organization("globex").await;

    let ambiguous = ran_by(&harness, &["workspace", "list"], Invocation::default()).await;
    exited(&ambiguous, UNRESOLVED);
    assert!(ambiguous.err.contains("kestrel status --organization acme"));
    assert!(
        ambiguous
            .err
            .contains("kestrel status --organization globex")
    );

    let empty = ran_by(
        &harness,
        &["workspace", "list"],
        Invocation::default().file(".kestrel/organization", ""),
    )
    .await;
    exited(&empty, UNRESOLVED);
    assert!(empty.err.contains("kestrel status --organization acme"));
    harness.teardown().await;
}

#[tokio::test]
async fn a_missing_workspace_names_the_setup_command_and_keeps_its_category() {
    let harness = an_organization().await;
    let finished = ran_by(
        &harness,
        &[
            "session",
            "open",
            "--workspace",
            "absent",
            "--agent",
            "agent",
        ],
        Invocation::default(),
    )
    .await;

    exited(&finished, UNRESOLVED);
    assert!(
        finished.err.contains("kestrel workspace declare absent"),
        "{}",
        finished.err
    );
    harness.teardown().await;
}

#[tokio::test]
async fn corrective_command_names_the_unencoded_organization() {
    let harness = Harness::boot().await;
    harness.declare_organization("Acme East").await;
    let finished = ran_by(
        &harness,
        &[
            "session",
            "open",
            "--workspace",
            "absent",
            "--agent",
            "agent",
        ],
        Invocation::default(),
    )
    .await;

    exited(&finished, UNRESOLVED);
    assert!(
        finished.err.contains("--organization 'Acme East'"),
        "{}",
        finished.err
    );
    harness.teardown().await;
}

#[tokio::test]
async fn a_run_in_the_session_names_the_run_to_stop_and_stays_rejected() {
    let harness = an_organization().await;
    let organization = harness.organizations().await.remove(0);
    let workspace = harness
        .declare_workspace(
            &organization,
            "work",
            &["https://example.com/repo".to_owned()],
            "main",
        )
        .await;
    let agent = harness
        .declare_agent(&organization, "worker", "opencode", None)
        .await;
    let session = harness
        .open_session("acme", &workspace.name, &agent.name)
        .await;
    let run = harness.enqueue_run(session.id).await;
    let finished = ran_by(
        &harness,
        &["run", "enqueue", "--session", &session.id.to_string()],
        Invocation::default(),
    )
    .await;

    exited(&finished, REJECTED);
    assert!(
        finished
            .err
            .contains(&format!("kestrel run stop {}", run.id)),
        "{}",
        finished.err
    );
    harness.teardown().await;
}

#[test]
fn credential_help_presents_set_and_forget_together() {
    let finished = ran(&nowhere(), &["credential", "--help"]);
    exited(&finished, SUCCESS);
    assert!(finished.out.join("\n").contains("credential set"));
    assert!(finished.out.join("\n").contains("credential forget"));
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
