mod support;

use std::time::Duration;

use jiff::SignedDuration;
use kestrel::domain::{
    Connection, Direction, GithubConnection, Integration, IntegrationId, OrganizationId, RunState,
};
use kestrel::integration::credential::Token;
use kestrel::integration::github::Github;
use kestrel::log::{Entry, Message};
use kestrel_scripted_agent::{FIRST_MEMORY, LAST_MEMORY};
use support::Kestrel;
use support::github_stub::{self, GithubStub};
use support::scripted_agent::Script;
use support::supervisor::Supervisor;
use support::{A_PROVIDER_KEY, PROVIDER_KEY};

const REPOSITORY: &str = "jtmthf/kestrel";
const MAINTAINER: &str = "jack";
const ISSUE: i64 = 43;
const EVENTS: &str = "/issues/events?";
const COMMENTS: &str = "/issues/comments?";
const PATIENCE: Duration = Duration::from_secs(30);

async fn a_session(kestrel: &Kestrel) -> kestrel::domain::Session {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(&organization, "kestrel", &[], "main")
        .await;
    kestrel
        .declare_agent(&organization, "builder", "opencode", None)
        .await;
    kestrel.open_session("acme", "kestrel", "builder").await
}

/// The Trigger comes before the poll: an Event recorded before the Trigger was declared fires
/// nothing.
async fn watching(kestrel: &Kestrel, stub: &GithubStub) {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(&organization, "kestrel", &[], "main")
        .await;
    kestrel
        .declare_agent(&organization, "builder", "opencode", None)
        .await;
    kestrel
        .declare_trigger(
            "acme",
            "ready",
            &support::labelled_on(REPOSITORY, "ready-for-agent"),
            "kestrel",
            "builder",
        )
        .await;
    kestrel
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            &[Direction::Inbound],
            SignedDuration::from_millis(1),
        )
        .await;
}

async fn sessions(kestrel: &Kestrel, count: usize) -> Vec<kestrel::domain::Session> {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let sessions = kestrel.sessions("acme").await;
        if sessions.len() == count {
            return sessions;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn runs(kestrel: &Kestrel, session: kestrel::domain::SessionId, count: usize) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if kestrel.runs(session).await.len() == count {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn message_arrived(kestrel: &Kestrel, session: kestrel::domain::SessionId, message: &str) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if kestrel.transcript(session).await.iter().any(|recorded| {
            matches!(&recorded.entry, Entry::Said { message: said, .. } if said == message)
        }) {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn requested(stub: &GithubStub, path: &str, after: usize) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let count = stub
            .requests()
            .iter()
            .filter(|request| request.url.contains(path))
            .count();
        if count > after {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn pending_arrived(kestrel: &Kestrel, session: kestrel::domain::SessionId) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if kestrel.has_pending_messages(session).await {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn posting_a_message_into_an_idle_session_enqueues_its_next_run() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;

    let run = kestrel
        .post(session.id, "operator", "please add the missing test")
        .await;

    assert_eq!(run.session, session.id);
    assert_eq!(run.state, RunState::Queued);
    assert!(kestrel.transcript(session.id).await.iter().any(|recorded| {
        matches!(
            &recorded.entry,
            Entry::Said { participant, message }
                if participant == "operator" && message == "please add the missing test"
        )
    }));

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_message_arriving_during_a_run_waits_for_that_run_to_end() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    let (active, _) = kestrel.dispatch_run(session.id).await;

    assert!(
        kestrel
            .post_while_busy(session.id, "operator", "one more change")
            .await
            .is_none()
    );
    assert!(
        kestrel
            .post_while_busy(session.id, "operator", "and update the docs")
            .await
            .is_none()
    );
    assert_eq!(kestrel.runs(session.id).await.len(), 1);
    assert!(
        !kestrel.transcript(session.id).await.iter().any(|recorded| {
            matches!(&recorded.entry, Entry::Said { .. } | Entry::Messages { .. })
        })
    );

    kestrel.complete_run(&active).await;

    let runs = kestrel.runs(session.id).await;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].state, RunState::Queued);
    let messages = kestrel
        .transcript(session.id)
        .await
        .into_iter()
        .filter_map(|recorded| match recorded.entry {
            Entry::Messages { messages } => Some(messages),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        vec![vec![
            Message {
                participant: "operator".to_owned(),
                message: "one more change".to_owned(),
            },
            Message {
                participant: "operator".to_owned(),
                message: "and update the docs".to_owned(),
            },
        ]]
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn cleanup_left_by_a_stopped_worker_is_found_before_the_session_continues() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    let (active, _) = kestrel.dispatch_run(session.id).await;
    kestrel.supervised(&active, "local-exec/2147483647").await;
    assert!(
        kestrel
            .post_while_busy(session.id, "operator", "continue after cleanup")
            .await
            .is_none()
    );

    kestrel.complete_run(&active).await;
    assert_eq!(kestrel.runs(session.id).await.len(), 1);
    let reapable = kestrel.supervisors_to_stop().await;
    assert_eq!(reapable.len(), 1);
    assert_eq!(reapable[0].0.id, active.id);

    kestrel.supervisor_gone(&active).await;
    assert_eq!(kestrel.runs(session.id).await.len(), 2);
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_cold_run_is_seeded_with_every_page_of_earlier_context() {
    let kestrel = Kestrel::boot().await;
    let session = a_session(&kestrel).await;
    let (first, _) = kestrel.dispatch_run(session.id).await;

    for index in 0..105 {
        let message = match index {
            0 => FIRST_MEMORY.to_owned(),
            104 => LAST_MEMORY.to_owned(),
            _ => format!("earlier message {index}"),
        };
        kestrel.said(&first, &message).await;
    }
    kestrel.complete_run(&first).await;
    let second = kestrel
        .post(session.id, "operator", "please continue")
        .await;
    let claimed = kestrel
        .claim_run()
        .await
        .expect("the second run should claim");
    assert_eq!(claimed.run.id, second.id);

    let mut supervisor = Supervisor::provision_playing(
        &kestrel.link(),
        second.id,
        &claimed.credential,
        Script::Recalls,
    );
    supervisor.wait_until_it_says("reported connected").await;
    kestrel.start(&second).await;
    supervisor.wait_until_it_says("reported answered").await;
    kestrel.stop_run(second.id).await;

    assert!(kestrel.transcript(session.id).await.iter().any(|recorded| {
        matches!(
            &recorded.entry,
            Entry::Said { message, .. } if message == "I remember the whole earlier context"
        )
    }));

    assert!(supervisor.finishes().await.success());
    kestrel.teardown().await;
}

#[tokio::test]
async fn the_second_run_starts_a_fresh_supervisor_on_the_same_instance_after_the_first_is_gone() {
    let kestrel = Kestrel::dispatching_to(
        support::supervisor::binary(),
        &support::scripted_agent::playing(Script::Lingers),
    )
    .await;
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(&organization, "kestrel", &[], "main")
        .await;
    kestrel
        .declare_agent(&organization, "builder", "opencode", None)
        .await;
    kestrel
        .hold_provider_credential(&organization, PROVIDER_KEY, A_PROVIDER_KEY)
        .await;
    let session = kestrel.open_session("acme", "kestrel", "builder").await;

    let first = kestrel.post(session.id, "operator", FIRST_MEMORY).await;
    kestrel.answered(first.id, 1).await;
    kestrel.stop_run(first.id).await;
    let first = kestrel.run(first.id).await;
    let first_supervisor = first.supervisor.as_deref().expect("a supervisor");
    support::environment::Environment::named(first_supervisor)
        .is_gone()
        .await;
    kestrel.supervisor_recorded_gone(&first).await;

    let second = kestrel.post(session.id, "operator", LAST_MEMORY).await;
    kestrel.answered(second.id, 1).await;
    let second = kestrel.run(second.id).await;
    let second_supervisor = second.supervisor.as_deref().expect("a supervisor");

    assert_ne!(first_supervisor, second_supervisor);
    assert_eq!(first.instance, second.instance);
    kestrel.stop_run(second.id).await;
    support::environment::Environment::named(second_supervisor)
        .is_gone()
        .await;

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_github_comment_enqueues_a_second_run_in_the_originating_session() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(
        7,
        ISSUE,
        "ready-for-agent",
    )]));
    let kestrel = Kestrel::boot().await;
    watching(&kestrel, &stub).await;
    let session = sessions(&kestrel, 1).await.remove(0);
    let first = kestrel
        .claim_run()
        .await
        .expect("the first run should claim")
        .run;

    let comments_before = stub
        .requests()
        .iter()
        .filter(|request| request.url.contains(COMMENTS))
        .count();
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(
            11,
            ISSUE,
            "jack",
            "please add the missing test",
        )]),
    );
    requested(&stub, COMMENTS, comments_before).await;
    pending_arrived(&kestrel, session.id).await;
    assert_eq!(
        kestrel.runs(session.id).await.len(),
        1,
        "the comment started a concurrent run"
    );
    assert!(!kestrel.transcript(session.id).await.iter().any(|recorded| {
        matches!(&recorded.entry, Entry::Said { message, .. } if message == "please add the missing test")
    }));

    kestrel.complete_run(&first).await;
    runs(&kestrel, session.id, 2).await;

    assert_eq!(kestrel.sessions("acme").await.len(), 1);
    assert!(kestrel.transcript(session.id).await.iter().any(|recorded| {
        matches!(
            &recorded.entry,
            Entry::Messages { messages }
                if messages == &[Message {
                    participant: "jack".to_owned(),
                    message: "please add the missing test".to_owned(),
                }]
        )
    }));

    kestrel.teardown().await;
}

#[tokio::test]
async fn comments_arriving_during_a_turn_wait_in_order_with_their_authors() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(
        7,
        ISSUE,
        "ready-for-agent",
    )]));
    let kestrel = Kestrel::boot().await;
    watching(&kestrel, &stub).await;
    let session = sessions(&kestrel, 1).await.remove(0);
    let first = kestrel
        .claim_run()
        .await
        .expect("the first run should claim")
        .run;

    let comments_before = stub
        .requests()
        .iter()
        .filter(|request| request.url.contains(COMMENTS))
        .count();
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[
            github_stub::issue_comment(12, ISSUE, "jill", "and update the docs"),
            github_stub::issue_comment(11, ISSUE, "jack", "one more change"),
        ]),
    );
    requested(&stub, COMMENTS, comments_before).await;
    pending_arrived(&kestrel, session.id).await;
    // Both comments are one poll's, and each is received in its own transaction; let the sweep
    // finish holding the second before the run ends.
    tokio::time::sleep(Duration::from_millis(300)).await;

    kestrel.complete_run(&first).await;
    runs(&kestrel, session.id, 2).await;

    assert!(kestrel.transcript(session.id).await.iter().any(|recorded| {
        recorded.entry
            == Entry::Messages {
                messages: vec![
                    Message {
                        participant: "jack".to_owned(),
                        message: "one more change".to_owned(),
                    },
                    Message {
                        participant: "jill".to_owned(),
                        message: "and update the docs".to_owned(),
                    },
                ],
            }
    }));

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_comment_polled_with_its_origin_waits_for_the_session_to_open() {
    let stub = GithubStub::start();
    let occurred_at = serde_json::json!("2026-09-01T12:00:07Z");
    let mut label = github_stub::labelled(7, ISSUE, "ready-for-agent");
    label["created_at"] = occurred_at.clone();
    let mut comment = github_stub::issue_comment(17, ISSUE, "jack", "picked up together");
    comment["created_at"] = occurred_at;
    stub.script_answer("GET", EVENTS, github_stub::page(&[label]));
    stub.script_answer("GET", COMMENTS, github_stub::page(&[comment]));

    let kestrel = Kestrel::boot().await;
    watching(&kestrel, &stub).await;
    let session = sessions(&kestrel, 1).await.remove(0);
    message_arrived(&kestrel, session.id, "picked up together").await;

    assert_eq!(kestrel.runs(session.id).await.len(), 1);
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_comment_backlog_larger_than_ten_pages_loses_nothing() {
    let stub = GithubStub::start();
    let comments = (100..1200)
        .rev()
        .map(|id| github_stub::issue_comment(id, ISSUE, "jack", &format!("comment {id}")))
        .collect::<Vec<_>>();
    for page in comments.chunks(100) {
        stub.script_answer("GET", COMMENTS, github_stub::page(page));
    }
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(
            99,
            ISSUE,
            "jack",
            "the previous watermark",
        )]),
    );
    let integration = Integration {
        id: IntegrationId::generate(),
        organization: OrganizationId::generate(),
        name: "github".to_owned(),
        connection: Connection::Github(GithubConnection {
            repository: REPOSITORY.to_owned(),
            api: stub.base_url(),
            credential: Token::held("not-a-secret"),
            interval: SignedDuration::from_secs(1),
            signed: false,
        }),
        carries: vec![Direction::Inbound],
        poll_due_at: None,
        polled_through: None,
        comments_polled_through: Some(99),
        last_event_refusal: None,
    };

    let seen = Github::dialling_out()
        .expect("the GitHub client")
        .issue_comments(&integration)
        .await
        .expect("the comment backlog should be read");

    assert_eq!(seen.occurrences.len(), 1100);
    assert_eq!(seen.through, Some(1199));
    assert_eq!(
        stub.requests()
            .iter()
            .filter(|request| request.url.contains(COMMENTS))
            .count(),
        12
    );
}

async fn watching_correlated(kestrel: &Kestrel, stub: &GithubStub, correlation: &str) {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(&organization, "kestrel", &[], "main")
        .await;
    kestrel
        .declare_agent(&organization, "builder", "opencode", None)
        .await;
    kestrel
        .declare_trigger_rendering(
            "acme",
            "ready",
            &support::labelled_on(REPOSITORY, "ready-for-agent"),
            "kestrel",
            "builder",
            &support::templates(support::BRIEF, None, Some(correlation)),
        )
        .await;
    kestrel
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            &[Direction::Inbound],
            SignedDuration::from_millis(1),
        )
        .await;
}

#[tokio::test]
async fn a_comment_on_a_sealed_session_feeds_the_open_one_holding_its_correlation() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(
        7,
        ISSUE,
        "ready-for-agent",
    )]));
    let kestrel = Kestrel::boot().await;
    watching_correlated(&kestrel, &stub, "the release").await;
    let sealed = sessions(&kestrel, 1).await.remove(0);
    let first = kestrel
        .claim_run()
        .await
        .expect("the first run should claim")
        .run;
    kestrel.complete_run(&first).await;
    kestrel.seal_session(sealed.id).await;

    stub.script_answer(
        "GET",
        EVENTS,
        github_stub::page(&[github_stub::labelled(8, ISSUE + 1, "ready-for-agent")]),
    );
    let holding = sessions(&kestrel, 2)
        .await
        .into_iter()
        .find(|session| session.id != sealed.id)
        .expect("the second label should continue the sealed session");
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(
            12,
            ISSUE,
            "jack",
            "about the release",
        )]),
    );

    message_arrived(&kestrel, holding.id, "about the release").await;
    assert_eq!(kestrel.sessions("acme").await.len(), 2);

    kestrel.teardown().await;
}

/// A Trigger that names the one login it obeys, so a Session it opened has an author it
/// authorizes and everyone else is a stranger to it.
async fn watching_a_named_actor(kestrel: &Kestrel, stub: &GithubStub) {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(&organization, "kestrel", &[], "main")
        .await;
    kestrel
        .declare_agent(&organization, "builder", "opencode", None)
        .await;
    kestrel
        .declare_trigger_rendering(
            "acme",
            "delegated",
            &serde_json::json!({"all": [
                {"exact": {"source": format!("https://github.com/{REPOSITORY}")}},
                {"exact": {"type": "com.github.issue_comment.created"}},
                {"exact": {"data.user.login": MAINTAINER}},
                {"prefix": {"data.body": "@kestrel"}},
            ]})
            .to_string(),
            "kestrel",
            "builder",
            &support::templates("Work on {{ event.subject }}", None, Some("the release")),
        )
        .await;
    kestrel
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            &[Direction::Inbound],
            SignedDuration::from_millis(1),
        )
        .await;
}

/// The maintainer's command, which is the comment that opens the session.
fn the_command(stub: &GithubStub) {
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(
            10, ISSUE, MAINTAINER, "@kestrel",
        )]),
    );
}

/// Scripted only once the run is already active, so a remark is judged against an open run
/// rather than taken as its first prompt.
fn a_remark_from(stub: &GithubStub, author: &str, remark: &str) {
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(11, ISSUE, author, remark)]),
    );
}

/// The sweep records the remark as an Event before it decides whether to feed it, so an Event
/// on the issue is the signal that the decision has been made.
async fn the_remark_was_recorded(kestrel: &Kestrel, remark: &str) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let recorded = kestrel.events("acme").await.iter().any(|event| {
            event
                .occurrence
                .data
                .get("body")
                .and_then(serde_json::Value::as_str)
                == Some(remark)
        });
        if recorded {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the remark was never polled"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_remark_from_a_stranger_does_not_feed_an_open_session() {
    let stub = GithubStub::start();
    the_command(&stub);
    let kestrel = Kestrel::boot().await;
    watching_a_named_actor(&kestrel, &stub).await;
    let session = sessions(&kestrel, 1).await.remove(0);
    let run = kestrel
        .claim_run()
        .await
        .expect("the command should have opened a run")
        .run;
    a_remark_from(&stub, "a-stranger", "please also change the parser");

    the_remark_was_recorded(&kestrel, "please also change the parser").await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !kestrel.has_pending_messages(session.id).await,
        "a stranger's remark was held as input to the run"
    );

    kestrel.complete_run(&run).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        kestrel.runs(session.id).await.len(),
        1,
        "a stranger's remark started a run"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_remark_from_the_trigger_actor_feeds_an_open_session() {
    let stub = GithubStub::start();
    the_command(&stub);
    let kestrel = Kestrel::boot().await;
    watching_a_named_actor(&kestrel, &stub).await;
    let session = sessions(&kestrel, 1).await.remove(0);
    let run = kestrel
        .claim_run()
        .await
        .expect("the command should have opened a run")
        .run;
    a_remark_from(&stub, MAINTAINER, "please also change the parser");

    pending_arrived(&kestrel, session.id).await;
    kestrel.complete_run(&run).await;
    runs(&kestrel, session.id, 2).await;

    assert!(kestrel.transcript(session.id).await.iter().any(|recorded| {
        recorded.entry
            == Entry::Messages {
                messages: vec![Message {
                    participant: MAINTAINER.to_owned(),
                    message: "please also change the parser".to_owned(),
                }],
            }
    }));

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_comment_kestrel_left_is_never_heard_as_input() {
    let stub = GithubStub::start();
    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(
            10, ISSUE, MAINTAINER, "@kestrel",
        )]),
    );
    let kestrel = Kestrel::boot().await;
    watching_a_named_actor(&kestrel, &stub).await;
    let session = sessions(&kestrel, 1).await.remove(0);
    kestrel
        .claim_run()
        .await
        .expect("the command should have opened a run");

    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(
            11,
            ISSUE,
            MAINTAINER,
            "what kestrel said\n\n<!-- kestrel run 01a0 turn 1 -->",
        )]),
    );
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        !kestrel.has_pending_messages(session.id).await,
        "kestrel heard its own comment as input"
    );
    assert_eq!(kestrel.runs(session.id).await.len(), 1);

    kestrel.teardown().await;
}
