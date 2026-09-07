mod support;

use std::time::Duration;

use jiff::SignedDuration;
use kestrel::domain::{Direction, Run, RunId, RunState};
use kestrel::link::Instruction;
use kestrel::log::Entry;
use kestrel_scripted_agent::{FIRST_MEMORY, LAST_MEMORY};
use support::Harness;
use support::github_stub::{self, GithubStub};
use support::scripted_agent::Script;
use support::supervisor::Supervisor;
use support::{A_PROVIDER_KEY, PROVIDER_KEY};

const REPOSITORY: &str = "jtmthf/kestrel";
const ISSUE: i64 = 43;
const COMMENTS: &str = "/issues/comments?";
const PATIENCE: Duration = Duration::from_secs(30);

async fn a_session(harness: &Harness) -> kestrel::domain::Session {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(&organization, "kestrel", &[], "main")
        .await;
    harness
        .declare_agent(&organization, "builder", "opencode", None)
        .await;
    harness.open_session("acme", "kestrel", "builder").await
}

async fn watching(harness: &Harness, stub: &GithubStub) {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(&organization, "kestrel", &[], "main")
        .await;
    harness
        .declare_agent(&organization, "builder", "opencode", None)
        .await;
    harness
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            &[Direction::Inbound],
            SignedDuration::from_millis(1),
        )
        .await;
    harness
        .declare_trigger(
            "acme",
            "ready",
            (REPOSITORY, "ready-for-agent"),
            "kestrel",
            "builder",
        )
        .await;
}

async fn sessions(harness: &Harness, count: usize) -> Vec<kestrel::domain::Session> {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let sessions = harness.sessions("acme").await;
        if sessions.len() == count {
            return sessions;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn runs(harness: &Harness, session: kestrel::domain::SessionId, count: usize) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if harness.runs(session).await.len() == count {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn message_arrived(harness: &Harness, session: kestrel::domain::SessionId, message: &str) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if harness.transcript(session).await.iter().any(|recorded| {
            matches!(&recorded.entry, Entry::Said { message: said, .. } if said == message)
        }) {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn ended(harness: &Harness, run: RunId) -> Run {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let run = harness.run(run).await;
        if run.state == RunState::Ended {
            return run;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn posting_a_message_into_an_idle_session_enqueues_its_next_run() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;

    let run = harness
        .post(session.id, "operator", "please add the missing test")
        .await;

    assert_eq!(run.session, session.id);
    assert_eq!(run.state, RunState::Queued);
    assert!(harness.transcript(session.id).await.iter().any(|recorded| {
        matches!(
            &recorded.entry,
            Entry::Said { participant, message }
                if participant == "operator" && message == "please add the missing test"
        )
    }));

    harness.teardown().await;
}

#[tokio::test]
async fn a_message_arriving_during_a_run_waits_for_that_run_to_end() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (active, _) = harness.dispatch_run(session.id).await;

    assert!(
        harness
            .post_while_busy(session.id, "operator", "one more change")
            .await
            .is_none()
    );
    assert_eq!(harness.runs(session.id).await.len(), 1);

    harness.complete_run(&active).await;

    let runs = harness.runs(session.id).await;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].state, RunState::Queued);

    harness.teardown().await;
}

#[tokio::test]
async fn a_cold_run_is_seeded_with_every_page_of_earlier_context() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (first, _) = harness.dispatch_run(session.id).await;

    for index in 0..105 {
        let message = match index {
            0 => FIRST_MEMORY.to_owned(),
            104 => LAST_MEMORY.to_owned(),
            _ => format!("earlier message {index}"),
        };
        harness.said(&first, &message).await;
    }
    harness.complete_run(&first).await;
    let second = harness
        .post(session.id, "operator", "please continue")
        .await;
    let claimed = harness
        .claim_run()
        .await
        .expect("the second run should claim");
    assert_eq!(claimed.run.id, second.id);

    let mut supervisor = Supervisor::provision_playing(
        &harness.link(),
        second.id,
        &claimed.credential,
        Script::Recalls,
    );
    supervisor.wait_until_it_says("reported connected").await;
    harness.instruct(&second, Instruction::Start).await;
    supervisor.wait_until_it_says("reported finished").await;

    assert!(harness.transcript(session.id).await.iter().any(|recorded| {
        matches!(
            &recorded.entry,
            Entry::Said { message, .. } if message == "I remember the whole earlier context"
        )
    }));

    assert!(supervisor.finishes().await.success());
    harness.teardown().await;
}

#[tokio::test]
async fn the_second_run_uses_a_fresh_environment_after_the_first_is_gone() {
    let harness = Harness::dispatching_to(
        support::supervisor::binary(),
        &support::scripted_agent::playing(Script::Recalls),
    )
    .await;
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(&organization, "kestrel", &[], "main")
        .await;
    harness
        .declare_agent(&organization, "builder", "scripted", None)
        .await;
    harness
        .hold_provider_credential(&organization, PROVIDER_KEY, A_PROVIDER_KEY)
        .await;
    let session = harness.open_session("acme", "kestrel", "builder").await;

    let first = harness.post(session.id, "operator", FIRST_MEMORY).await;
    let first = ended(&harness, first.id).await;
    let first_environment = first.environment.as_deref().expect("an environment");
    support::environment::Environment::named(first_environment)
        .is_gone()
        .await;

    let second = harness.post(session.id, "operator", LAST_MEMORY).await;
    let second = ended(&harness, second.id).await;
    let second_environment = second.environment.as_deref().expect("an environment");

    assert_ne!(first_environment, second_environment);
    support::environment::Environment::named(second_environment)
        .is_gone()
        .await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_github_comment_enqueues_a_second_run_in_the_originating_session() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(
        7,
        ISSUE,
        "ready-for-agent",
    )]));
    let harness = Harness::boot().await;
    watching(&harness, &stub).await;
    let session = sessions(&harness, 1).await.remove(0);
    let first = harness
        .claim_run()
        .await
        .expect("the first run should claim")
        .run;

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
    message_arrived(&harness, session.id, "please add the missing test").await;
    assert_eq!(
        harness.runs(session.id).await.len(),
        1,
        "the comment started a concurrent run"
    );

    harness.complete_run(&first).await;
    runs(&harness, session.id, 2).await;

    assert_eq!(harness.sessions("acme").await.len(), 1);
    assert!(harness.transcript(session.id).await.iter().any(|recorded| {
        matches!(
            &recorded.entry,
            Entry::Said { participant, message }
                if participant == "jack" && message == "please add the missing test"
        )
    }));

    harness.teardown().await;
}

#[tokio::test]
async fn a_github_comment_after_sealing_opens_a_continuation() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(
        7,
        ISSUE,
        "ready-for-agent",
    )]));
    let harness = Harness::boot().await;
    watching(&harness, &stub).await;
    let sealed = sessions(&harness, 1).await.remove(0);
    let first = harness
        .claim_run()
        .await
        .expect("the first run should claim")
        .run;
    harness.complete_run(&first).await;
    harness.seal_session(sealed.id).await;

    stub.script_answer(
        "GET",
        COMMENTS,
        github_stub::page(&[github_stub::issue_comment(
            12,
            ISSUE,
            "jack",
            "continue after the seal",
        )]),
    );
    let opened = sessions(&harness, 2).await;
    let continuation = opened
        .iter()
        .find(|session| session.id != sealed.id)
        .expect("a continuation should open");

    assert_eq!(continuation.continues, Some(sealed.id));
    assert_eq!(harness.runs(sealed.id).await.len(), 1);
    assert_eq!(harness.runs(continuation.id).await.len(), 1);

    harness.teardown().await;
}
