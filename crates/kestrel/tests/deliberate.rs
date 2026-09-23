//! The declarations under test are the ones kestrel is dogfooded with.

mod support;

use std::time::Duration;

use jiff::SignedDuration;
use kestrel::domain::{Direction, Session, SessionId};
use kestrel::log::Entry;
use kestrel::trigger::{Asked, Fired};
use support::Harness;
use support::github_stub::{self, GithubStub};

const DOGFOOD: &str = include_str!("../../../.kestrel/triggers.yaml");
const REPOSITORY: &str = "jtmthf/kestrel";
const MAINTAINER: &str = "jtmthf";
const KESTREL: &str = "kestrel";
const EVENTS: &str = "/issues/events?";
const COMMENTS: &str = "/issues/comments?";
const PATIENCE: Duration = Duration::from_secs(30);

async fn dogfooding(harness: &Harness, stub: &GithubStub) {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(&organization, "kestrel", &[], "main")
        .await;
    for (agent, runtime) in [
        ("builder", "opencode"),
        ("codex", "codex"),
        ("claude", "claude"),
    ] {
        harness
            .declare_agent(&organization, agent, runtime, None)
            .await;
    }
    let applied = harness.apply_triggers("acme", DOGFOOD).await;
    assert!(
        applied.admitting_outsiders.is_empty(),
        "{:?}",
        applied.admitting_outsiders
    );
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
}

async fn sessions(harness: &Harness, count: usize) -> Vec<Session> {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let sessions = harness.sessions("acme").await;
        if sessions.len() >= count {
            return sessions;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{count} sessions never opened"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn brief(harness: &Harness, session: SessionId) -> String {
    harness
        .transcript(session)
        .await
        .into_iter()
        .find_map(|recorded| match recorded.entry {
            Entry::Brief { brief, .. } => Some(brief),
            _ => None,
        })
        .expect("a session opened by a trigger starts with its brief")
}

async fn said(harness: &Harness, session: SessionId) -> Vec<(String, String)> {
    harness
        .transcript(session)
        .await
        .into_iter()
        .flat_map(|recorded| match recorded.entry {
            Entry::Said {
                participant,
                message,
            } => vec![(participant, message)],
            Entry::Messages { messages } => messages
                .into_iter()
                .map(|message| (message.participant, message.message))
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn issue_link(issue: i64) -> String {
    format!("https://github.com/{REPOSITORY}/issues/{issue}")
}

#[tokio::test]
async fn neither_labels_nor_assignment_start_work() {
    let stub = GithubStub::start();
    stub.script_answer(
        "GET",
        EVENTS,
        github_stub::page(&[
            github_stub::assigned(14, 45, KESTREL, MAINTAINER),
            github_stub::labelled_carrying(12, 42, "agent:codex", &["ready-for-agent"]),
            github_stub::labelled(11, 41, "ready-for-agent"),
        ]),
    );
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(20, 43, MAINTAINER, "@kestrel")]),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let opened = sessions(&harness, 1).await;

    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].agent.name, "builder");
    assert_eq!(opened[0].checkout.branch, "kestrel/issue-43");
    assert_eq!(
        brief(&harness, opened[0].id).await,
        format!(
            "/implement {}\n\nRead the issue and its comments with `gh issue view --comments` \
             before you start.",
            issue_link(43)
        )
    );
    for event in harness.events("acme").await {
        if event.occurrence.subject.as_deref() != Some("#43") {
            assert!(
                harness.firings(event.record_id).await.is_empty(),
                "{} fired",
                event.occurrence.r#type
            );
        }
    }

    harness.teardown().await;
}

#[tokio::test]
async fn the_maintainers_mention_starts_work_with_the_instruction_and_agent_it_names() {
    let stub = GithubStub::start();
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(
            20,
            50,
            MAINTAINER,
            "@kestrel agent=codex $tdd the parser",
        )]),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let opened = sessions(&harness, 1).await.remove(0);

    assert_eq!(opened.agent.name, "codex");
    assert!(
        brief(&harness, opened.id)
            .await
            .starts_with(&format!("$tdd the parser {}\n", issue_link(50)))
    );

    harness.teardown().await;
}

#[tokio::test]
async fn ordinary_comments_strangers_and_kestrel_itself_command_nothing() {
    let stub = GithubStub::start();
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[
            github_stub::issue_comment(26, 55, MAINTAINER, "@kestrel"),
            github_stub::issue_comment(25, 56, MAINTAINER, "@kestrel-bot can you look?"),
            github_stub::issue_comment(24, 54, MAINTAINER, "thanks @kestrel"),
            github_stub::issue_comment(
                23,
                53,
                MAINTAINER,
                "@kestrel done\n<!-- kestrel run 01a0 -->",
            ),
            github_stub::issue_comment(22, 52, "a-stranger", "@kestrel /implement"),
            github_stub::issue_comment(21, 51, MAINTAINER, "this one is ready"),
        ]),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let opened = sessions(&harness, 1).await;

    assert_eq!(opened.len(), 1);
    assert!(
        brief(&harness, opened[0].id)
            .await
            .contains(&issue_link(55))
    );
    for event in harness.events("acme").await {
        assert_ne!(
            event.occurrence.subject.as_deref(),
            Some("#53"),
            "kestrel heard its own comment"
        );
        let firings = harness.firings(event.record_id).await;
        match event.occurrence.subject.as_deref() {
            Some("#55") => {}
            Some("#56") => assert!(
                firings.iter().all(|firing| firing.outcome == "failed"),
                "{firings:?}"
            ),
            _ => assert!(firings.is_empty()),
        }
    }

    harness.teardown().await;
}

#[tokio::test]
async fn repeated_signals_for_one_issue_open_one_session() {
    let stub = GithubStub::start();
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[
            github_stub::issue_comment(22, 43, MAINTAINER, "@kestrel /implement"),
            github_stub::issue_comment(21, 43, MAINTAINER, "@kestrel"),
            github_stub::issue_comment(20, 43, MAINTAINER, "@kestrel"),
        ]),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let deadline = tokio::time::Instant::now() + PATIENCE;
    let outcomes = loop {
        let events = harness.events("acme").await;
        let mut outcomes = Vec::new();
        for event in &events {
            outcomes.extend(
                harness
                    .firings(event.record_id)
                    .await
                    .into_iter()
                    .map(|firing| firing.outcome),
            );
        }
        if events.len() == 3 && outcomes.len() == 3 {
            break outcomes;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "not every signal fired"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    assert_eq!(harness.sessions("acme").await.len(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| *outcome == "opened")
            .count(),
        1,
        "{outcomes:?}"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_command_on_an_open_sessions_issue_is_not_also_heard_as_a_remark() {
    let stub = GithubStub::start();
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(10, 43, MAINTAINER, "@kestrel")]),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;
    let session = sessions(&harness, 1).await.remove(0);
    let first = harness.claim_run().await.expect("the first run").run;
    harness.complete_run(&first).await;

    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[
            github_stub::issue_comment(21, 43, MAINTAINER, "and a test, please"),
            github_stub::issue_comment(20, 43, MAINTAINER, "@kestrel /again"),
        ]),
    );

    let deadline = tokio::time::Instant::now() + PATIENCE;
    let heard = loop {
        let heard = said(&harness, session.id).await;
        let commanded = heard
            .iter()
            .any(|(by, message)| by == "delegated" && message.starts_with("/again "));
        let remarked = heard
            .iter()
            .any(|(by, message)| by == MAINTAINER && message == "and a test, please");
        if commanded && remarked {
            break heard;
        }
        if let Some(claimed) = harness.claim_run().await {
            harness.complete_run(&claimed.run).await;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the comments were never heard: {heard:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    assert!(
        !heard
            .iter()
            .any(|(_, message)| message.starts_with("@kestrel")),
        "{heard:?}"
    );
    assert_eq!(harness.sessions("acme").await.len(), 1);

    harness.teardown().await;
}

#[tokio::test]
async fn a_comment_on_a_sealed_sessions_issue_starts_nothing_and_a_command_continues_it() {
    let stub = GithubStub::start();
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(10, 43, MAINTAINER, "@kestrel")]),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;
    let sealed = sessions(&harness, 1).await.remove(0);
    let first = harness.claim_run().await.expect("the first run").run;
    harness.complete_run(&first).await;
    harness.seal_session(sealed.id).await;

    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[
            github_stub::issue_comment(21, 43, MAINTAINER, "@kestrel /again"),
            github_stub::issue_comment(20, 43, MAINTAINER, "still broken"),
        ]),
    );
    let opened = sessions(&harness, 2).await;

    assert_eq!(opened.len(), 2);
    let continuation = opened
        .iter()
        .find(|session| session.id != sealed.id)
        .expect("the command continues the sealed session");
    assert_eq!(continuation.continues, Some(sealed.id));
    assert_eq!(continuation.correlation, sealed.correlation);
    assert!(
        brief(&harness, continuation.id)
            .await
            .starts_with("/again ")
    );
    assert!(said(&harness, continuation.id).await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_dispatch_starts_the_work_it_asks_for_on_the_issue_it_names() {
    let stub = GithubStub::start();
    stub.script_answer(
        "GET",
        "/issues/60",
        github_stub::issue(60, &["agent:claude"]),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let fired = harness
        .dispatch(
            "acme",
            "delegated",
            60,
            Asked {
                instruction: Some("/tdd the parser"),
                agent: Some("codex"),
            },
        )
        .await
        .expect("the dispatch should fire");

    let Fired::Opened { session, .. } = fired else {
        panic!("the dispatch opened nothing: {fired:?}");
    };
    let session = harness.show_session(session).await;
    assert_eq!(session.agent.name, "codex");
    assert_eq!(session.checkout.branch, "kestrel/issue-60");
    assert!(
        brief(&harness, session.id)
            .await
            .starts_with(&format!("/tdd the parser {}\n", issue_link(60)))
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_dispatch_fires_only_the_trigger_it_names() {
    let stub = GithubStub::start();
    stub.script_answer("GET", "/issues/60", github_stub::issue(60, &[]));
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;
    harness
        .declare_trigger(
            "acme",
            "everything",
            &format!(r#"{{"exact": {{"source": "https://github.com/{REPOSITORY}"}}}}"#),
            "kestrel",
            "builder",
        )
        .await;

    harness
        .dispatch("acme", "delegated", 60, Asked::default())
        .await
        .expect("the dispatch should fire");
    stub.script_answer(
        "GET",
        EVENTS,
        github_stub::page(&[github_stub::labelled(7, 61, "bug")]),
    );

    let opened = sessions(&harness, 2).await;
    assert_eq!(opened.len(), 2);
    let dispatched = harness
        .events("acme")
        .await
        .into_iter()
        .find(|event| event.occurrence.r#type == kestrel::trigger::DISPATCHED)
        .expect("the dispatch is recorded");
    let firings = harness.firings(dispatched.record_id).await;
    assert_eq!(firings.len(), 1);
    assert_eq!(firings[0].trigger, "delegated");

    harness.teardown().await;
}

#[tokio::test]
async fn a_dispatch_asking_for_an_agent_the_trigger_does_not_allow_starts_nothing() {
    let stub = GithubStub::start();
    stub.script_answer("GET", "/issues/60", github_stub::issue(60, &[]));
    let harness = Harness::boot().await;
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_agent(&organization, "stranger", "opencode", None)
        .await;
    dogfooding(&harness, &stub).await;

    let fired = harness
        .dispatch(
            "acme",
            "delegated",
            60,
            Asked {
                instruction: None,
                agent: Some("stranger"),
            },
        )
        .await
        .expect("the dispatch should be recorded");

    let Fired::Failed { because, .. } = fired else {
        panic!("the dispatch started work: {fired:?}");
    };
    assert_eq!(
        because,
        "the trigger delegated does not allow the agent stranger that was asked for"
    );
    assert!(harness.sessions("acme").await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_blocker_added_after_a_command_holds_the_start() {
    let stub = GithubStub::start();
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(
            20,
            43,
            MAINTAINER,
            "@kestrel /implement",
        )]),
    );
    stub.script_answer("GET", "/issues/43", github_stub::issue(43, &[]));
    stub.script_answer(
        "GET",
        "/issues/43/dependencies/blocked_by",
        github_stub::page(&[serde_json::json!({
            "number": 42,
            "state": "open",
            "html_url": issue_link(42),
        })]),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let firing = command_firing(&harness).await;

    assert_eq!(firing.outcome, "held");
    assert!(firing.failure.unwrap_or_default().contains(&issue_link(42)));
    assert!(harness.sessions("acme").await.is_empty());

    harness.teardown().await;
}

async fn command_firing(harness: &Harness) -> kestrel::domain::Firing {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        for event in harness.events("acme").await {
            if event.occurrence.subject.as_deref() == Some("#43")
                && let Some(firing) = harness.firings(event.record_id).await.into_iter().next()
            {
                return firing;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the command did not fire"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn script_command(stub: &GithubStub) {
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(20, 43, MAINTAINER, "@kestrel")]),
    );
}

#[tokio::test]
async fn a_closed_issue_holds_a_stale_command() {
    let stub = GithubStub::start();
    script_command(&stub);
    stub.script_answer(
        "GET",
        "/issues/43",
        github_stub::ScriptedResponse::ok(
            serde_json::json!({
                "number": 43,
                "state": "closed",
                "assignees": [{ "login": KESTREL }],
            })
            .to_string(),
        ),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let firing = command_firing(&harness).await;
    assert_eq!(firing.outcome, "held");
    assert!(firing.failure.unwrap_or_default().contains("is closed"));
    assert!(harness.sessions("acme").await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn an_issue_with_unknown_state_holds_the_start() {
    let stub = GithubStub::start();
    script_command(&stub);
    stub.script_answer(
        "GET",
        "/issues/43",
        github_stub::ScriptedResponse::ok(
            serde_json::json!({
                "number": 43,
                "assignees": [{ "login": KESTREL }],
            })
            .to_string(),
        ),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let firing = command_firing(&harness).await;
    assert_eq!(firing.outcome, "held");
    assert!(firing.failure.unwrap_or_default().contains("unknown state"));
    assert!(harness.sessions("acme").await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn an_edited_command_without_a_current_assignment_holds_the_start() {
    let stub = GithubStub::start();
    script_command(&stub);
    stub.script_answer(
        "GET",
        "/issues/43",
        github_stub::ScriptedResponse::ok(
            serde_json::json!({
                "number": 43,
                "state": "open",
                "assignees": [],
            })
            .to_string(),
        ),
    );
    stub.script_answer(
        "GET",
        "/issues/comments/20",
        github_stub::ScriptedResponse::ok(
            github_stub::issue_comment(20, 43, MAINTAINER, "never mind").to_string(),
        ),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let firing = command_firing(&harness).await;
    assert_eq!(firing.outcome, "held");
    assert!(
        firing
            .failure
            .unwrap_or_default()
            .contains("no longer delegated")
    );
    assert!(harness.sessions("acme").await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_failed_dependency_query_holds_the_start() {
    let stub = GithubStub::start();
    script_command(&stub);
    stub.script_answer(
        "GET",
        "/issues/43/dependencies/blocked_by",
        github_stub::ScriptedResponse::answering(503),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let firing = command_firing(&harness).await;
    assert_eq!(firing.outcome, "held");
    assert!(
        firing
            .failure
            .unwrap_or_default()
            .contains("readiness could not be checked")
    );
    assert!(harness.sessions("acme").await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_current_command_can_start_an_unassigned_issue() {
    let stub = GithubStub::start();
    script_command(&stub);
    stub.script_answer(
        "GET",
        "/issues/43",
        github_stub::ScriptedResponse::ok(
            serde_json::json!({
                "number": 43,
                "state": "open",
                "assignees": [],
            })
            .to_string(),
        ),
    );
    stub.script_answer(
        "GET",
        "/issues/comments/20",
        github_stub::ScriptedResponse::ok(
            github_stub::issue_comment(20, 43, MAINTAINER, "@kestrel").to_string(),
        ),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let opened = sessions(&harness, 1).await;
    assert_eq!(opened.len(), 1);
    assert_eq!(command_firing(&harness).await.outcome, "opened");

    harness.teardown().await;
}

#[tokio::test]
async fn a_closed_native_dependency_does_not_hold_the_start() {
    let stub = GithubStub::start();
    script_command(&stub);
    stub.script_answer(
        "GET",
        "/issues/43/dependencies/blocked_by",
        github_stub::page(&[serde_json::json!({
            "number": 42,
            "state": "closed",
            "html_url": issue_link(42),
        })]),
    );
    let harness = Harness::boot().await;
    dogfooding(&harness, &stub).await;

    let opened = sessions(&harness, 1).await;
    assert_eq!(opened.len(), 1);
    assert_eq!(command_firing(&harness).await.outcome, "opened");

    harness.teardown().await;
}
