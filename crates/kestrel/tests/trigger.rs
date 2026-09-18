mod support;

use std::time::Duration;

use jiff::SignedDuration;
use kestrel::domain::{CorrelationMiss, Direction, Event, RunState, Session, TriggerState};
use kestrel::log::{Entry, Message};
use kestrel::trigger::Rendered;
use support::github_stub::{self, GithubStub};
use support::scripted_agent::Script;
use support::supervisor::Supervisor;
use support::{A_PROVIDER_KEY, Harness, PROVIDER_KEY, labelled_on, templates};

const PATIENCE: Duration = Duration::from_secs(30);
const REPOSITORY: &str = "jtmthf/kestrel";
const READY: &str = "ready-for-agent";
const EVENTS: &str = "/issues/events?";
const BOTH: &[Direction] = &[Direction::Inbound, Direction::Outbound];

/// Sooner than the wheel's own sweep, so what paces these tests is the sweep rather than a
/// wait written into them.
fn eagerly() -> SignedDuration {
    SignedDuration::from_millis(1)
}

/// An organization with somewhere for work to happen and someone to do it. The Trigger is the
/// one thing each test declares for itself.
async fn an_organization(harness: &Harness, name: &str) -> kestrel::domain::Organization {
    let organization = harness.declare_organization(name).await;
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
    organization
}

/// The poll that records what happens on the repository, started after the Trigger the test
/// is about: an Event recorded before a Trigger was declared fires nothing.
async fn watching(harness: &Harness, stub: &GithubStub) {
    harness
        .register_integration(
            "acme",
            "github",
            REPOSITORY,
            &stub.base_url(),
            BOTH,
            eagerly(),
        )
        .await;
}

async fn ready_for_agent(harness: &Harness) {
    harness
        .declare_trigger(
            "acme",
            "ready",
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "builder",
        )
        .await;
}

async fn opened(harness: &Harness, count: usize) -> Vec<Session> {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let sessions = harness.sessions("acme").await;
        if sessions.len() >= count {
            return sessions;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{count} sessions were never opened, only {}",
            sessions.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn recorded(harness: &Harness, count: usize) -> Vec<Event> {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let events = harness.events("acme").await;
        if events.len() >= count {
            return events;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the repository's events were never recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Nothing opening is only observable by waiting for the sweeps that would have opened it, so
/// this waits for the Event to be recorded and then for several sweeps to pass over it.
async fn nothing_opens(harness: &Harness) {
    recorded(harness, 1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        harness.sessions("acme").await.is_empty(),
        "a session was opened for an event nothing should have fired on"
    );
}

#[tokio::test]
async fn labelling_an_issue_opens_a_session_and_enqueues_a_run() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let session = opened(&harness, 1).await.remove(0);

    assert_eq!(session.workspace.name, "kestrel");
    assert_eq!(session.agent.name, "builder");

    let runs = harness.runs(session.id).await;
    assert_eq!(runs.len(), 1, "a firing enqueues one run");
    assert_eq!(runs[0].state, RunState::Queued);

    harness.teardown().await;
}

async fn ready_rendering(
    harness: &Harness,
    name: &str,
    brief: &str,
    branch: Option<&str>,
    correlation: Option<&str>,
) {
    harness
        .declare_trigger_rendering(
            "acme",
            name,
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "builder",
            &templates(brief, branch, correlation),
        )
        .await;
}

async fn first_entry(harness: &Harness, session: &Session) -> Entry {
    harness
        .transcript(session.id)
        .await
        .into_iter()
        .next()
        .expect("a triggered session has a transcript")
        .entry
}

#[tokio::test]
async fn the_rendered_brief_is_the_sessions_first_transcript_entry() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_rendering(
        &harness,
        "ready",
        "Work {{ event.data.issue.html_url }}: {{ event.data.issue.title }}",
        None,
        None,
    )
    .await;
    watching(&harness, &stub).await;

    let session = opened(&harness, 1).await.remove(0);
    let transcript = harness.transcript(session.id).await;

    assert_eq!(
        transcript
            .iter()
            .map(|entry| entry.entry.clone())
            .collect::<Vec<_>>(),
        [
            Entry::Brief {
                trigger: "ready".to_owned(),
                brief: "Work https://github.com/jtmthf/kestrel/issues/43: an issue numbered 43"
                    .to_owned(),
            },
            Entry::ParticipantJoined {
                participant: "builder".to_owned(),
            },
        ]
    );

    harness.teardown().await;
}

const SKILLED: &str = "/implement https://github.com/jtmthf/kestrel/issues/43\n\n\
                       Fetch its current body and comments with `gh issue view --comments` first.";

/// Opened by a firing whose brief leads with a harness's skill invocation, in a workspace a
/// supervisor can check out without reaching GitHub.
async fn briefed(harness: &Harness, stub: &GithubStub) -> Session {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(&organization, "kestrel", &[], "main")
        .await;
    harness
        .declare_agent(&organization, "builder", "opencode", None)
        .await;
    harness
        .hold_provider_credential(&organization, PROVIDER_KEY, A_PROVIDER_KEY)
        .await;
    ready_rendering(
        harness,
        "ready",
        "/implement {{ event.data.issue.html_url }}\n\n\
         Fetch its current body and comments with `gh issue view --comments` first.",
        None,
        None,
    )
    .await;
    watching(harness, stub).await;

    opened(harness, 1).await.remove(0)
}

/// What the agent was prompted with, which the echoing agent says back.
async fn prompted(harness: &Harness, session: &Session) -> String {
    let claimed = harness
        .claim_run()
        .await
        .expect("the firing's run should claim");
    let mut supervisor = Supervisor::provision_playing(
        &harness.link(),
        claimed.run.id,
        &claimed.credential,
        Script::Echoes,
    );
    supervisor.wait_until_it_says("reported connected").await;
    harness.start(&claimed.run).await;
    supervisor.wait_until_it_says("reported finished").await;
    assert!(supervisor.finishes().await.success());

    harness
        .transcript(session.id)
        .await
        .into_iter()
        .find_map(|recorded| match recorded.entry {
            Entry::Said {
                participant,
                message,
            } if participant == "builder" => Some(message),
            _ => None,
        })
        .expect("the agent should say what it was prompted with")
}

#[tokio::test]
async fn the_agent_is_first_prompted_with_exactly_the_brief_its_session_preserved() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    let session = briefed(&harness, &stub).await;

    assert_eq!(
        first_entry(&harness, &session).await,
        Entry::Brief {
            trigger: "ready".to_owned(),
            brief: SKILLED.to_owned(),
        }
    );
    assert_eq!(prompted(&harness, &session).await, SKILLED);

    harness.teardown().await;
}

#[tokio::test]
async fn a_brief_something_was_said_after_reaches_the_agent_as_earlier_context() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    let session = briefed(&harness, &stub).await;
    harness
        .post_while_busy(session.id, "operator", "and add a test")
        .await;

    let prompt = prompted(&harness, &session).await;

    assert!(prompt.starts_with("Earlier context"), "{prompt}");
    assert!(
        prompt.contains("/implement https://github.com/jtmthf/kestrel/issues/43"),
        "{prompt}"
    );
    assert!(prompt.contains("and add a test"), "{prompt}");

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_opens_on_the_branch_and_correlation_its_trigger_renders() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_rendering(
        &harness,
        "ready",
        support::BRIEF,
        Some("kestrel/issue-{{ event.data.issue.number }}"),
        Some("{{ event.source }}{{ event.subject }}"),
    )
    .await;
    watching(&harness, &stub).await;

    let session = opened(&harness, 1).await.remove(0);
    let shown = harness.show_session(session.id).await;

    assert_eq!(shown.checkout.branch, "kestrel/issue-43");
    assert_eq!(
        shown.correlation.as_deref(),
        Some("https://github.com/jtmthf/kestrel#43")
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_whose_trigger_renders_no_branch_opens_on_its_own_cut_from_the_workspaces() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let session = opened(&harness, 1).await.remove(0);
    let shown = harness.show_session(session.id).await;

    assert_eq!(shown.checkout.branch, format!("kestrel/{}", session.id));
    assert_eq!(shown.checkout.base, "main");
    assert_eq!(shown.correlation, None);

    harness.teardown().await;
}

/// A failed firing is recorded rather than retried, so it neither opens a Session on a later
/// sweep nor holds up another Trigger matching the same Event.
#[tokio::test]
async fn a_brief_that_cannot_render_fails_the_firing_and_starts_nothing() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_rendering(
        &harness,
        "review",
        "Review the pull request on {{ event.data.pull_request.head.ref }}",
        None,
        None,
    )
    .await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    opened(&harness, 1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let sessions = harness.sessions("acme").await;
    assert_eq!(
        sessions.len(),
        1,
        "only the trigger that renders opens work"
    );
    let Entry::Brief { trigger, .. } = first_entry(&harness, &sessions[0]).await else {
        panic!("a triggered session opens on its brief");
    };
    assert_eq!(trigger, "ready");

    harness.teardown().await;
}

#[tokio::test]
async fn a_correlation_hit_feeds_the_open_session_without_changing_its_agent() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    let organization = an_organization(&harness, "acme").await;
    let correlation = Some("{{ event.source }}{{ event.subject }}");
    ready_rendering(&harness, "ready", support::BRIEF, None, correlation).await;
    harness
        .declare_agent(&organization, "reviewer", "opencode", None)
        .await;
    harness
        .declare_trigger_rendering(
            "acme",
            "also-ready",
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "reviewer",
            &templates(support::BRIEF, None, correlation),
        )
        .await;
    watching(&harness, &stub).await;

    opened(&harness, 1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let sessions = harness.sessions("acme").await;
    assert_eq!(
        sessions.len(),
        1,
        "one correlation opened {} sessions",
        sessions.len()
    );
    assert_eq!(sessions[0].agent.name, "builder");
    assert!(
        harness
            .transcript(sessions[0].id)
            .await
            .iter()
            .any(|recorded| {
                matches!(
                    &recorded.entry,
                    Entry::Said { participant, message }
                        if participant == "also-ready" && message == "Work on an issue numbered 43"
                )
            })
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_correlation_miss_can_be_ignored() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    harness
        .declare_trigger_rendering_with_miss(
            "acme",
            "ready",
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "builder",
            &templates(
                support::BRIEF,
                None,
                Some("{{ event.source }}{{ event.subject }}"),
            ),
            Some(CorrelationMiss::Ignore),
        )
        .await;
    watching(&harness, &stub).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_correlated_trigger_must_declare_what_it_does_on_a_miss() {
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;

    let refusal = harness
        .try_declare_trigger_rendering_with_miss(
            "acme",
            "ready",
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "builder",
            &templates(
                support::BRIEF,
                None,
                Some("{{ event.source }}{{ event.subject }}"),
            ),
            None,
        )
        .await
        .expect_err("a correlated trigger without a miss behavior should be refused");

    assert!(refusal.to_string().contains("must declare"));

    harness.teardown().await;
}

#[tokio::test]
async fn a_correlation_miss_opens_a_continuation_of_the_sealed_session() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_rendering(
        &harness,
        "ready",
        support::BRIEF,
        None,
        Some("{{ event.source }}{{ event.subject }}"),
    )
    .await;
    watching(&harness, &stub).await;

    let sealed = opened(&harness, 1).await.remove(0);
    let active = harness
        .claim_run()
        .await
        .expect("the firing enqueued a run")
        .run;
    harness.complete_run(&active).await;
    harness.seal_session(sealed.id).await;

    // Scripted for the events endpoint alone: the comment the completed run posts would
    // otherwise take this response off the shared queue.
    stub.script_answer(
        "GET",
        EVENTS,
        github_stub::page(&[github_stub::labelled(8, 43, READY)]),
    );
    let sessions = opened(&harness, 2).await;
    let continuation = sessions
        .into_iter()
        .find(|session| session.id != sealed.id)
        .expect("a new session should open after the seal");

    assert_eq!(continuation.continues, Some(sealed.id));
    assert_eq!(continuation.checkout.branch, sealed.checkout.branch);
    assert_eq!(continuation.state, kestrel::domain::SessionState::Open);

    harness.teardown().await;
}

#[tokio::test]
async fn an_ignoring_trigger_still_continues_a_sealed_session_it_correlates_to() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    let correlation = templates(
        support::BRIEF,
        None,
        Some("{{ event.source }}{{ event.subject }}"),
    );
    harness
        .declare_trigger_rendering_with_miss(
            "acme",
            "ready",
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "builder",
            &correlation,
            Some(CorrelationMiss::Open),
        )
        .await;
    harness
        .declare_trigger_rendering_with_miss(
            "acme",
            "failing",
            &labelled_on(REPOSITORY, "ci-failed"),
            "kestrel",
            "builder",
            &correlation,
            Some(CorrelationMiss::Ignore),
        )
        .await;
    watching(&harness, &stub).await;

    let sealed = opened(&harness, 1).await.remove(0);
    let active = harness
        .claim_run()
        .await
        .expect("the firing enqueued a run")
        .run;
    harness.complete_run(&active).await;
    harness.seal_session(sealed.id).await;

    stub.script_answer(
        "GET",
        EVENTS,
        github_stub::page(&[github_stub::labelled(8, 43, "ci-failed")]),
    );
    let continuation = opened(&harness, 2)
        .await
        .into_iter()
        .find(|session| session.id != sealed.id)
        .expect("the ignoring trigger should continue the sealed session");

    assert_eq!(continuation.continues, Some(sealed.id));
    assert_eq!(continuation.correlation, sealed.correlation);

    harness.teardown().await;
}

#[tokio::test]
async fn correlated_events_arriving_during_a_run_drain_into_one_entry_and_one_run() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_rendering(
        &harness,
        "ready",
        support::BRIEF,
        None,
        Some("{{ event.source }}{{ event.subject }}"),
    )
    .await;
    watching(&harness, &stub).await;

    let session = opened(&harness, 1).await.remove(0);
    let active = harness
        .claim_run()
        .await
        .expect("the firing enqueued a run")
        .run;
    stub.script(github_stub::page(&[
        github_stub::labelled(9, 43, READY),
        github_stub::labelled(8, 43, READY),
    ]));
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !harness.has_pending_messages(session.id).await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "correlated events never arrived"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(harness.runs(session.id).await.len(), 1);
    harness.complete_run(&active).await;

    let runs = harness.runs(session.id).await;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].state, RunState::Queued);
    let messages = harness
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
                participant: "ready".to_owned(),
                message: "Work on an issue numbered 43".to_owned(),
            },
            Message {
                participant: "ready".to_owned(),
                message: "Work on an issue numbered 43".to_owned(),
            },
        ]]
    );

    harness.teardown().await;
}

#[tokio::test]
async fn the_session_records_the_event_that_started_it() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let session = opened(&harness, 1).await.remove(0);
    let events = harness.events("acme").await;

    assert_eq!(session.started_by, Some(events[0].record_id));

    harness.teardown().await;
}

/// The same label going on the same issue twice is one Event however many polls see it, and
/// one firing however many sweeps pass over it.
#[tokio::test]
async fn relabelling_the_same_issue_twice_opens_exactly_one_session() {
    let stub = GithubStub::start();
    let relabelled = github_stub::page(&[github_stub::labelled(7, 43, READY)]);
    stub.script(relabelled.clone());
    stub.script(relabelled);
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    opened(&harness, 1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let sessions = harness.sessions("acme").await;
    assert_eq!(
        sessions.len(),
        1,
        "one event opened {} sessions",
        sessions.len()
    );

    harness.teardown().await;
}

#[tokio::test]
async fn an_event_matching_several_triggers_fires_every_one_of_them() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    harness
        .declare_trigger(
            "acme",
            "anything-labelled",
            r#"{"exact": {"type": "com.github.issues.labeled"}}"#,
            "kestrel",
            "builder",
        )
        .await;
    watching(&harness, &stub).await;

    let sessions = opened(&harness, 2).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut fired = Vec::new();
    for session in harness.sessions("acme").await {
        if let Entry::Brief { trigger, .. } = first_entry(&harness, &session).await {
            fired.push(trigger);
        }
    }
    fired.sort();
    assert_eq!(sessions.len(), 2);
    assert_eq!(fired, ["anything-labelled", "ready"]);

    harness.teardown().await;
}

#[tokio::test]
async fn an_event_matching_no_trigger_opens_nothing() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(
        7,
        43,
        "needs-triage",
    )]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

/// GitHub reports the label coming off as an `unlabeled` event carrying that same label, and
/// a trigger that fired on it would start work every time someone tidied an issue up.
#[tokio::test]
async fn taking_the_label_back_off_fires_nothing() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::unlabelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_disabled_trigger_fires_for_nothing() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    harness.disable_trigger("acme", "ready").await;
    watching(&harness, &stub).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_trigger_that_exceeds_its_firing_budget_disables_without_stopping_another() {
    let stub = GithubStub::start();
    let events = (7..18)
        .map(|id| github_stub::labelled(id, id + 36, READY))
        .collect::<Vec<_>>();
    stub.script(github_stub::page(&events));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    harness
        .declare_trigger(
            "acme",
            "other",
            &labelled_on(REPOSITORY, "needs-triage"),
            "kestrel",
            "builder",
        )
        .await;
    watching(&harness, &stub).await;

    opened(&harness, 10).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(harness.sessions("acme").await.len(), 10);
    let disabled = harness.show_trigger("acme", "ready").await;
    assert!(matches!(disabled.state, TriggerState::Disabled(_)));
    assert!(
        disabled
            .disabled_because
            .as_deref()
            .is_some_and(|because| because.contains("exhausted its budget"))
    );
    assert_eq!(
        harness.show_trigger("acme", "other").await.state,
        TriggerState::Enabled
    );

    harness.enable_trigger("acme", "ready").await;
    let manually_disabled = harness.disable_trigger("acme", "ready").await;
    assert_eq!(
        manually_disabled.disabled_because.as_deref(),
        Some("disabled by an operator")
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_trigger_enabled_after_exhausting_its_budget_fires_again_within_the_window() {
    let stub = GithubStub::start();
    let events = (7..18)
        .map(|id| github_stub::labelled(id, id + 36, READY))
        .collect::<Vec<_>>();
    stub.script(github_stub::page(&events));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    opened(&harness, 10).await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while harness.show_trigger("acme", "ready").await.state == TriggerState::Enabled {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the trigger never exhausted its budget"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    harness.enable_trigger("acme", "ready").await;
    stub.script(github_stub::page(&[github_stub::labelled(30, 66, READY)]));

    opened(&harness, 11).await;
    assert_eq!(
        harness.show_trigger("acme", "ready").await.state,
        TriggerState::Enabled
    );

    harness.teardown().await;
}

/// Disabling stops a Trigger firing without forgetting it, so what it was declared to match
/// is still there to be enabled again.
#[tokio::test]
async fn a_trigger_is_named_listed_and_disabled() {
    let stub = GithubStub::start();
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;
    watching(&harness, &stub).await;

    let listed = harness.triggers("acme").await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "ready");
    assert_eq!(
        listed[0].fires.to_string(),
        r#"source = "https://github.com/jtmthf/kestrel" and type = "com.github.issues.labeled" and data.label.name = "ready-for-agent""#
    );
    assert_eq!(listed[0].workspace.name, "kestrel");
    assert_eq!(listed[0].agent.name, "builder");
    assert_eq!(listed[0].state, TriggerState::Enabled);

    assert!(matches!(
        harness.disable_trigger("acme", "ready").await.state,
        TriggerState::Disabled(_)
    ));
    assert!(matches!(
        harness.show_trigger("acme", "ready").await.state,
        TriggerState::Disabled(_)
    ));
    assert_eq!(
        harness.enable_trigger("acme", "ready").await.state,
        TriggerState::Enabled
    );

    harness.teardown().await;
}

/// Turning automation on never works a backlog. A Trigger declared against a repository whose
/// history kestrel already holds opens nothing for that history, however many sweeps pass over
/// it; catching one up is a deliberate act, and declaring a Trigger is not it.
#[tokio::test]
async fn a_trigger_never_fires_for_events_recorded_before_it_was_declared() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[
        github_stub::labelled(9, 45, READY),
        github_stub::labelled(8, 44, READY),
        github_stub::labelled(7, 43, READY),
    ]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    watching(&harness, &stub).await;

    recorded(&harness, 3).await;
    ready_for_agent(&harness).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

fn applying_ready_for(label: &str) -> String {
    format!(
        r#"
triggers:
  ready:
    filter:
      all:
        - exact: {{source: "https://github.com/{REPOSITORY}"}}
        - exact: {{type: com.github.issues.labeled}}
        - exact: {{data.label.name: {label}}}
    brief: "Work on {{{{ event.data.issue.title }}}}"
    workspace: kestrel
    agent: builder
"#
    )
}

/// The first apply in a repository kestrel has watched for a month must not open a session for
/// every issue that month labelled.
#[tokio::test]
async fn applying_a_declaration_file_never_fires_for_events_already_recorded() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[
        github_stub::labelled(9, 45, READY),
        github_stub::labelled(8, 44, READY),
    ]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    watching(&harness, &stub).await;

    recorded(&harness, 2).await;
    harness
        .apply_triggers("acme", &applying_ready_for(READY))
        .await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

/// Widening what a trigger matches is not a way to reach back for the events the narrower one
/// passed over.
#[tokio::test]
async fn reapplying_a_changed_filter_never_fires_for_events_already_recorded() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(
        7,
        43,
        "needs-triage",
    )]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    harness
        .apply_triggers("acme", &applying_ready_for(READY))
        .await;
    watching(&harness, &stub).await;

    recorded(&harness, 1).await;
    harness
        .apply_triggers("acme", &applying_ready_for("needs-triage"))
        .await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

/// An applied trigger is the same rule a declared one is: it fires for what arrives after it,
/// and removing it leaves the work it started alone.
#[tokio::test]
async fn an_applied_trigger_fires_for_events_recorded_after_it() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    harness
        .apply_triggers("acme", &applying_ready_for(READY))
        .await;
    watching(&harness, &stub).await;

    let session = opened(&harness, 1).await.remove(0);
    assert_eq!(session.agent.name, "builder");

    harness.apply_triggers("acme", "triggers: {}").await;
    assert!(harness.triggers("acme").await.is_empty());
    assert_eq!(harness.sessions("acme").await.len(), 1);

    harness.teardown().await;
}

#[tokio::test]
async fn a_trigger_fires_only_for_the_source_it_names() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    harness
        .declare_trigger(
            "acme",
            "elsewhere",
            &labelled_on("globex/other", READY),
            "kestrel",
            "builder",
        )
        .await;
    watching(&harness, &stub).await;

    nothing_opens(&harness).await;

    harness.teardown().await;
}

/// A dry run asks only whether the filter matches, so it answers for an Event recorded before
/// the Trigger was declared, which is the one kind a Trigger never fires for.
#[tokio::test]
async fn trigger_test_says_whether_a_recorded_event_matches() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    watching(&harness, &stub).await;
    let event = recorded(&harness, 1).await.remove(0).record_id;

    for (at, (filter, matches)) in [
        (r#"{"exact": {"type": "com.github.issues.labeled"}}"#, true),
        (
            r#"{"exact": {"type": "com.github.issues.unlabeled"}}"#,
            false,
        ),
        (r#"{"exact": {"type": "com.github.issues"}}"#, false),
        (
            r#"{"prefix": {"source": "https://github.com/jtmthf/"}}"#,
            true,
        ),
        (
            r#"{"prefix": {"source": "https://github.com/globex/"}}"#,
            false,
        ),
        (r#"{"suffix": {"subject": "43"}}"#, true),
        (r#"{"suffix": {"subject": "44"}}"#, false),
        (r#"{"exact": {"data.label.name": "ready-for-agent"}}"#, true),
        (r#"{"prefix": {"data.label.name": "ready-"}}"#, true),
        (r#"{"prefix": {"data.label.name": "READY-"}}"#, false),
        (r#"{"prefix": {"data.label.name": "ready_"}}"#, false),
        (r#"{"suffix": {"data.label.name": "%agent"}}"#, false),
        (r#"{"exact": {"data.issue.number": "43"}}"#, true),
        (r#"{"exact": {"data.actor": "jtmthf"}}"#, false),
        (r#"{"exact": {"data.milestone.title": "v1"}}"#, false),
        (
            r#"{"not": {"exact": {"data.milestone.title": "v1"}}}"#,
            true,
        ),
        (
            r#"{"not": {"exact": {"type": "com.github.issues.labeled"}}}"#,
            false,
        ),
        (
            r#"{"all": [
                {"exact": {"type": "com.github.issues.labeled"}},
                {"exact": {"data.label.name": "needs-triage"}}
            ]}"#,
            false,
        ),
        (
            r#"{"any": [
                {"exact": {"data.label.name": "needs-triage"}},
                {"exact": {"data.label.name": "ready-for-agent"}}
            ]}"#,
            true,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("case-{at}");
        harness
            .declare_trigger("acme", &name, filter, "kestrel", "builder")
            .await;

        assert_eq!(
            harness.test_trigger("acme", &name, event).await.matches,
            matches,
            "{filter} should {}match the labelled event",
            if matches { "" } else { "not " }
        );
    }

    harness.teardown().await;
}

/// An Event belongs to one Organization, and another Organization's Trigger cannot so much as
/// ask whether it would have matched.
#[tokio::test]
async fn a_trigger_is_tested_only_against_its_own_organizations_events() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    an_organization(&harness, "globex").await;
    harness
        .declare_trigger(
            "globex",
            "ready",
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "builder",
        )
        .await;
    watching(&harness, &stub).await;
    let event = recorded(&harness, 1).await.remove(0).record_id;

    let refusal = harness
        .try_test_trigger("globex", "ready", event)
        .await
        .expect_err("another organization's event should be refused");

    assert!(
        refusal.to_string().contains("globex"),
        "unhelpful refusal: {refusal}"
    );

    harness.teardown().await;
}

/// What a firing would hand its Session is visible before anything runs.
#[tokio::test]
async fn trigger_test_renders_the_brief_and_resolves_the_branch() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    watching(&harness, &stub).await;
    let event = recorded(&harness, 1).await.remove(0).record_id;
    harness
        .declare_trigger_rendering(
            "acme",
            "ready",
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "builder",
            &templates(
                "Work {{ event.data.issue.html_url }}: {{ event.data.issue.title }}",
                Some("kestrel/issue-{{ event.data.issue.number }}"),
                Some("{{ event.source }}{{ event.subject }}"),
            ),
        )
        .await;

    let tested = harness.test_trigger("acme", "ready", event).await;

    assert!(tested.matches);
    assert_eq!(
        tested.rendered.expect("the trigger should render"),
        Rendered {
            brief: "Work https://github.com/jtmthf/kestrel/issues/43: an issue numbered 43"
                .to_owned(),
            branch: Some("kestrel/issue-43".to_owned()),
            correlation: Some("https://github.com/jtmthf/kestrel#43".to_owned()),
        }
    );

    harness.teardown().await;
}

/// A declaration under review is worth testing before it is applied, and testing it applies
/// nothing.
#[tokio::test]
async fn trigger_test_answers_for_a_declaration_not_yet_applied() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    watching(&harness, &stub).await;
    let event = recorded(&harness, 1).await.remove(0).record_id;

    let tested = harness
        .test_declared_trigger("acme", &applying_ready_for(READY), "ready", event)
        .await;

    assert!(tested.matches);
    assert_eq!(
        tested.rendered.expect("the trigger should render"),
        Rendered {
            brief: "Work on an issue numbered 43".to_owned(),
            branch: None,
            correlation: None,
        }
    );
    assert!(harness.triggers("acme").await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_trigger_that_renders_no_branch_leaves_the_session_its_own() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    watching(&harness, &stub).await;
    let event = recorded(&harness, 1).await.remove(0).record_id;
    ready_for_agent(&harness).await;

    let rendered = harness
        .test_trigger("acme", "ready", event)
        .await
        .rendered
        .expect("the trigger should render");

    assert_eq!(rendered.branch, None);
    assert_eq!(rendered.correlation, None);

    harness.teardown().await;
}

/// A labelled issue has no pull request, and a brief that assumes one is a failure rather
/// than a run that starts on nothing.
#[tokio::test]
async fn a_brief_that_cannot_render_fails_naming_the_trigger_and_the_event() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    watching(&harness, &stub).await;
    let event = recorded(&harness, 1).await.remove(0).record_id;
    harness
        .declare_trigger_rendering(
            "acme",
            "review",
            &labelled_on(REPOSITORY, READY),
            "kestrel",
            "builder",
            &templates(
                "Review the pull request on {{ event.data.pull_request.head.ref }}",
                None,
                None,
            ),
        )
        .await;

    let tested = harness.test_trigger("acme", "review", event).await;
    let failure = format!(
        "{:#}",
        tested
            .rendered
            .expect_err("a brief over a missing field should not render")
    );

    assert!(tested.matches);
    assert!(
        failure.contains(&format!(
            "the trigger review cannot render its brief for the event {event}"
        )),
        "the failure does not name both: {failure}"
    );
    assert!(
        failure.contains("undefined value"),
        "the failure does not say why: {failure}"
    );

    harness.teardown().await;
}

async fn hourly(harness: &Harness, brief: &str) -> kestrel::domain::Trigger {
    harness
        .try_declare_scheduled_trigger(
            "acme",
            "sweep",
            SignedDuration::from_hours(1),
            &templates(brief, Some("kestrel/sweep-{{ event.id[:13] }}"), None),
        )
        .await
        .expect("an hourly schedule should declare")
}

#[tokio::test]
async fn a_schedule_elapsing_opens_a_session_the_way_a_matched_event_does() {
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    let trigger = hourly(&harness, "Sweep the backlog for {{ event.data.trigger }}").await;

    let minted = harness
        .elapse(trigger.declared_at + SignedDuration::from_hours(1))
        .await;
    let session = opened(&harness, 1).await.remove(0);

    assert_eq!(minted.len(), 1, "one schedule elapsed once");
    let event = harness.events("acme").await.remove(0);
    assert_eq!(event.integration, None, "kestrel minted it");
    assert_eq!(
        event.occurrence.source,
        format!("urn:kestrel:trigger:{}", trigger.id)
    );
    assert_eq!(event.occurrence.r#type, "dev.kestrel.schedule.elapsed");
    assert_eq!(
        event.occurrence.time,
        trigger.declared_at + SignedDuration::from_hours(1)
    );
    assert_eq!(session.started_by, Some(event.record_id));
    assert_eq!(session.agent.name, "builder");
    assert_eq!(
        first_entry(&harness, &session).await,
        Entry::Brief {
            trigger: "sweep".to_owned(),
            brief: "Sweep the backlog for sweep".to_owned(),
        }
    );
    assert_eq!(harness.runs(session.id).await.len(), 1);

    harness.teardown().await;
}

#[tokio::test]
async fn elapsings_missed_while_nothing_swept_elapse_once() {
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    let trigger = hourly(&harness, "Sweep the backlog").await;
    let hours = |count| trigger.declared_at + SignedDuration::from_hours(count);

    assert_eq!(harness.elapse(hours(5)).await.len(), 1);
    assert!(
        harness.elapse(hours(5)).await.is_empty(),
        "the next elapsing is not due until the sixth hour"
    );
    let next = harness.elapse(hours(6)).await;

    assert_eq!(next.len(), 1);
    assert_eq!(next[0].time, hours(6));

    harness.teardown().await;
}

#[tokio::test]
async fn a_disabled_schedule_does_not_elapse() {
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    let trigger = hourly(&harness, "Sweep the backlog").await;
    harness.disable_trigger("acme", "sweep").await;

    let minted = harness
        .elapse(trigger.declared_at + SignedDuration::from_hours(3))
        .await;

    assert!(minted.is_empty());
    assert!(harness.events("acme").await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_schedule_faster_than_the_firing_budget_is_refused() {
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;

    let refusal = harness
        .try_declare_scheduled_trigger(
            "acme",
            "impatient",
            SignedDuration::from_mins(1),
            &templates("Sweep the backlog", None, None),
        )
        .await
        .expect_err("a schedule that exhausts its budget should be refused");

    assert!(
        format!("{refusal:#}").contains("fire at most every 6m"),
        "the refusal does not say what would do: {refusal:#}"
    );
    assert!(harness.triggers("acme").await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_scheduled_trigger_is_tested_against_its_next_elapsing() {
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    let trigger = hourly(&harness, "Sweep the backlog due {{ event.time }}").await;
    let due = trigger.declared_at + SignedDuration::from_hours(1);

    let tested = harness.test_scheduled_trigger("acme", "sweep").await;

    assert!(tested.matches);
    assert_eq!(tested.elapsing, Some(due));
    let rendered = tested.rendered.expect("the brief should render");
    assert_eq!(rendered.brief, format!("Sweep the backlog due {due}"));
    assert_eq!(
        rendered.branch,
        Some(format!("kestrel/sweep-{}", &due.to_string()[..13]))
    );
    assert!(
        harness.events("acme").await.is_empty(),
        "a test records nothing"
    );
    assert!(harness.sessions("acme").await.is_empty());

    harness.teardown().await;
}

#[tokio::test]
async fn a_scheduled_trigger_matches_what_its_own_schedule_minted_and_nothing_else() {
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(7, 43, READY)]));
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    let trigger = hourly(&harness, "Sweep the backlog").await;
    harness
        .elapse(trigger.declared_at + SignedDuration::from_hours(1))
        .await;
    watching(&harness, &stub).await;
    let events = recorded(&harness, 2).await;
    let (minted, labelled): (Vec<_>, Vec<_>) = events
        .into_iter()
        .partition(|event| event.integration.is_none());

    assert!(
        harness
            .test_trigger("acme", "sweep", minted[0].record_id)
            .await
            .matches
    );
    assert!(
        !harness
            .test_trigger("acme", "sweep", labelled[0].record_id)
            .await
            .matches
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_webhook_naming_a_schedule_does_not_elapse_it() {
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    let trigger = hourly(&harness, "Sweep the backlog").await;
    let webhook = harness
        .register_webhook("acme", "ci", "a-shared-secret")
        .await;
    let forged = serde_json::json!({
        "id": "forged",
        "source": format!("urn:kestrel:trigger:{}", trigger.id),
        "specversion": "1.0",
        "type": "dev.kestrel.schedule.elapsed",
        "time": trigger.declared_at.to_string(),
    });
    let answered = reqwest::Client::new()
        .post(format!("{}{}", harness.link(), webhook.webhook_path()))
        .bearer_auth("a-shared-secret")
        .header("content-type", "application/cloudevents+json")
        .body(forged.to_string())
        .send()
        .await
        .expect("the webhook answers");
    assert!(answered.status().is_success());
    let forged = recorded(&harness, 1).await.remove(0);

    harness
        .elapse(trigger.declared_at + SignedDuration::from_hours(1))
        .await;
    let session = opened(&harness, 1).await.remove(0);

    assert!(
        !harness
            .test_trigger("acme", "sweep", forged.record_id)
            .await
            .matches
    );
    assert_ne!(session.started_by, Some(forged.record_id));
    assert_eq!(harness.sessions("acme").await.len(), 1);

    harness.teardown().await;
}

#[tokio::test]
async fn a_trigger_that_fires_on_events_is_tested_against_a_named_one() {
    let harness = Harness::boot().await;
    an_organization(&harness, "acme").await;
    ready_for_agent(&harness).await;

    let refusal = harness
        .try_test_trigger_naming_no_event("acme", "ready")
        .await
        .expect_err("a test of a matching trigger needs an event");

    assert!(format!("{refusal:#}").contains("so a test names one"));

    harness.teardown().await;
}
