//! What a Run says back: each completed Turn's response reaches the issue the work came from,
//! promptly and once, and the Run's own ending is said only when it adds something (ADR-0024).

mod support;

use std::time::Duration;

use jiff::SignedDuration;
use kestrel::domain::{Direction, Exit, Run, RunState, Session, SessionId};
use kestrel::link::credential::Secret;
use kestrel::work::{Report, Reported};
use support::Harness;
use support::RUNTIME;
use support::github_stub::{self, GithubStub, RecordedRequest, ScriptedResponse};
use support::link_client::Link;

const PATIENCE: Duration = Duration::from_secs(30);
const REPOSITORY: &str = "jtmthf/kestrel";
const ISSUE: i64 = 43;
const COMMENTS: &str = "/issues/43/comments";

fn comments(stub: &GithubStub) -> Vec<RecordedRequest> {
    stub.requests()
        .into_iter()
        .filter(|request| request.method == "POST" && request.url.contains(COMMENTS))
        .collect()
}

fn said(comment: &RecordedRequest) -> String {
    serde_json::from_str::<serde_json::Value>(&comment.body)
        .expect("a comment is posted as json")
        .get("body")
        .and_then(serde_json::Value::as_str)
        .expect("a comment carries a body")
        .to_owned()
}

/// The bodies posted to the issue once at least `count` of them have been.
async fn replies(stub: &GithubStub, count: usize) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let comments = comments(stub);
        if comments.len() >= count {
            return comments.iter().map(said).collect();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "only {} of {count} replies reached the issue",
            comments.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Nothing being said is only observable by waiting for the sweeps that would have said it.
async fn nothing_more_is_said(stub: &GithubStub, after: usize) {
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        comments(stub).len(),
        after,
        "a reply that should not have been said out loud was"
    );
}

async fn sessions(harness: &Harness, count: usize) -> Vec<Session> {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let sessions = harness.sessions("acme").await;
        if sessions.len() == count {
            return sessions;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{count} sessions never opened"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A Session an Event started through an Integration that carries what it says back out.
async fn a_session_from_the_issue(harness: &Harness, stub: &GithubStub) -> Session {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(&organization, "kestrel", &[], "main")
        .await;
    harness
        .declare_agent(&organization, "builder", RUNTIME, None)
        .await;
    harness
        .declare_trigger(
            "acme",
            "ready",
            &support::labelled_on(REPOSITORY, "ready-for-agent"),
            "kestrel",
            "builder",
        )
        .await;
    harness
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            &[Direction::Inbound, Direction::Outbound],
            SignedDuration::from_millis(1),
        )
        .await;
    stub.script(github_stub::page(&[github_stub::labelled(
        7,
        ISSUE,
        "ready-for-agent",
    )]));

    sessions(harness, 1).await.remove(0)
}

/// A Run claimed the way a work role claims it, with its first Turn prompted.
async fn a_working_run(harness: &Harness, session: SessionId) -> (Run, Secret) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if harness.runs(session).await.len() == 1
            && let Some(claimed) = harness.claim_run().await
        {
            harness.start(&claimed.run).await;
            return (claimed.run, claimed.credential);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the session never had a run to claim"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn report(link: &Link, run: &Run, credential: &Secret, seq: i64, report: Report) {
    let answered = link
        .report(
            run.id,
            Some(credential),
            &Reported {
                seq: Some(seq),
                report,
            },
        )
        .await;
    assert_eq!(
        answered.status(),
        reqwest::StatusCode::ACCEPTED,
        "the link refused report {seq}"
    );
}

#[tokio::test]
async fn a_turns_response_reaches_the_issue_before_the_run_ends() {
    let stub = GithubStub::start();
    stub.script_answer("POST", COMMENTS, github_stub::created(1, "posted"));
    let harness = Harness::boot().await;
    let session = a_session_from_the_issue(&harness, &stub).await;
    let (run, credential) = a_working_run(&harness, session.id).await;
    let link = Link::to(&harness.link());

    report(&link, &run, &credential, 1, Report::Started).await;
    report(
        &link,
        &run,
        &credential,
        2,
        Report::Said {
            message: "the first answer".to_owned(),
        },
    )
    .await;
    report(&link, &run, &credential, 3, Report::Answered).await;

    let bodies = replies(&stub, 1).await;
    assert!(bodies[0].contains("the first answer"), "{}", bodies[0]);
    assert!(
        bodies[0].contains(&format!("run {} turn 1 -->", run.id)),
        "the reply does not carry this turn's marker: {}",
        bodies[0]
    );
    assert_eq!(harness.run(run.id).await.state, RunState::Active);

    harness.stop_run(run.id).await;
    nothing_more_is_said(&stub, 1).await;

    harness.teardown().await;
}

#[tokio::test]
async fn each_turn_of_one_run_says_its_own_response_once() {
    let stub = GithubStub::start();
    stub.script_answer("POST", COMMENTS, github_stub::created(1, "posted"));
    let harness = Harness::boot().await;
    let session = a_session_from_the_issue(&harness, &stub).await;
    let (run, credential) = a_working_run(&harness, session.id).await;
    let link = Link::to(&harness.link());

    report(&link, &run, &credential, 1, Report::Started).await;
    report(
        &link,
        &run,
        &credential,
        2,
        Report::Said {
            message: "the first answer".to_owned(),
        },
    )
    .await;
    report(&link, &run, &credential, 3, Report::Answered).await;
    let bodies = replies(&stub, 1).await;
    assert!(bodies[0].contains("the first answer"), "{}", bodies[0]);

    // The next Turn waits on the Run holding no slot, so the work role prompts it with what
    // arrived in between.
    harness
        .post_while_busy(session.id, "operator", "the second thing to do")
        .await
        .expect("a run between turns takes the next prompt");
    harness.prompt_waiting().await;
    report(
        &link,
        &run,
        &credential,
        4,
        Report::Said {
            message: "the second answer".to_owned(),
        },
    )
    .await;
    report(&link, &run, &credential, 5, Report::Answered).await;

    let bodies = replies(&stub, 2).await;
    assert!(bodies[1].contains("the second answer"), "{}", bodies[1]);
    assert!(
        bodies[0] != bodies[1] && bodies[1].contains("turn 2 -->"),
        "the second turn said the first's words: {bodies:?}"
    );
    assert_eq!(
        harness.turns(run.id).await.len(),
        2,
        "a turn was prompted more than once"
    );

    harness.stop_run(run.id).await;
    nothing_more_is_said(&stub, 2).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_that_answered_no_turn_still_says_how_it_ended() {
    let stub = GithubStub::start();
    stub.script_answer("POST", COMMENTS, github_stub::created(1, "posted"));
    let harness = Harness::boot().await;
    let session = a_session_from_the_issue(&harness, &stub).await;
    let (run, credential) = a_working_run(&harness, session.id).await;
    let link = Link::to(&harness.link());

    report(&link, &run, &credential, 1, Report::Started).await;
    report(
        &link,
        &run,
        &credential,
        2,
        Report::Finished {
            exit: Exit::Succeeded,
        },
    )
    .await;

    let bodies = replies(&stub, 1).await;
    assert!(bodies[0].contains("run succeeded"), "{}", bodies[0]);
    assert!(
        bodies[0].contains(&format!("run {} -->", run.id)),
        "the outcome does not carry the run's marker: {}",
        bodies[0]
    );

    harness.teardown().await;
}

/// A Turn that fails the Run still posts the Turn it already answered, and the failure is said
/// because the exit status is information the responses did not carry.
#[tokio::test]
async fn a_failed_run_posts_its_turns_response_and_then_the_failure() {
    let stub = GithubStub::start();
    stub.script_answer("POST", COMMENTS, github_stub::created(1, "posted"));
    let harness = Harness::boot().await;
    let session = a_session_from_the_issue(&harness, &stub).await;
    let (run, credential) = a_working_run(&harness, session.id).await;
    let link = Link::to(&harness.link());

    report(&link, &run, &credential, 1, Report::Started).await;
    report(
        &link,
        &run,
        &credential,
        2,
        Report::Said {
            message: "the first answer".to_owned(),
        },
    )
    .await;
    report(&link, &run, &credential, 3, Report::Answered).await;
    replies(&stub, 1).await;

    report(
        &link,
        &run,
        &credential,
        4,
        Report::Finished {
            exit: Exit::Failed {
                because: "the agent answered the prompt with nothing".to_owned(),
            },
        },
    )
    .await;

    let bodies = replies(&stub, 2).await;
    assert!(bodies[1].contains("run failed"), "{}", bodies[1]);
    assert!(
        bodies[1].contains("the agent answered the prompt with nothing"),
        "{}",
        bodies[1]
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_turn_response_that_landed_while_the_control_plane_died_is_not_posted_twice() {
    let stub = GithubStub::start();
    stub.script_answer("POST", COMMENTS, ScriptedResponse::answering(502));
    let harness = Harness::boot().await;
    let session = a_session_from_the_issue(&harness, &stub).await;
    let (run, credential) = a_working_run(&harness, session.id).await;
    let link = Link::to(&harness.link());

    report(&link, &run, &credential, 1, Report::Started).await;
    report(
        &link,
        &run,
        &credential,
        2,
        Report::Said {
            message: "the answer that landed".to_owned(),
        },
    )
    .await;
    report(&link, &run, &credential, 3, Report::Answered).await;
    let landed = replies(&stub, 1).await.remove(0);
    assert!(landed.contains("the answer that landed"), "{landed}");

    // The read-back the retry recognises its own comment by is queued before the control plane
    // comes back, so the window in which it could post a second one has nothing in it.
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::comment(1, &landed)]),
    );
    let harness = harness.kill_and_restart().await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        comments(&stub).len(),
        1,
        "the turn response already on the issue was posted again after the restart"
    );

    harness.teardown().await;
}
