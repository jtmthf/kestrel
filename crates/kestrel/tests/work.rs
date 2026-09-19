//! A Run from enqueued to ended, driven through the primary test seam: the work role claims
//! it, a local-exec Environment executes it, and it ends with an exit status.

mod support;

use std::path::Path;
use std::time::Duration;

use kestrel::domain::{Exit, Run, RunId, RunState, Session};
use kestrel::work::{Report, Reported};
use support::Harness;
use support::environment::Environment;
use support::link_client::Link;
use support::repository;
use support::scripted_agent::{self, Script};
use support::supervisor;

const PATIENCE: Duration = Duration::from_secs(30);

async fn a_session(harness: &Harness) -> Session {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &organization,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;
    harness
        .declare_agent(
            &organization,
            "builder",
            "opencode",
            Some(kestrel_scripted_agent::OTHER_MODEL),
        )
        .await;

    harness
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;

    harness.open_session("acme", "kestrel", "builder").await
}

async fn until(harness: &Harness, run: RunId, what: &str, ready: impl Fn(&Run) -> bool) -> Run {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let run = harness.run(run).await;
        if ready(&run) {
            return run;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run {} is {} with the exit status {:?}, and never {what}",
            run.id,
            run.state,
            run.exit
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Answering a turn never ends a Run, so one that answered is stopped, the way a person would.
async fn ended(harness: &Harness, run: RunId) -> Run {
    harness.after_one_turn(run).await
}

#[tokio::test]
async fn runs_in_distinct_sessions_start_at_the_same_time() {
    let harness = Harness::dispatching_up_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Dawdles),
        2,
    )
    .await;
    let first_session = a_session(&harness).await;
    let second_session = harness.open_session("acme", "kestrel", "builder").await;
    let first = harness.enqueue_run(first_session.id).await;
    let second = harness.enqueue_run(second_session.id).await;

    let second = until(&harness, second.id, "started", |run| {
        run.started_at.is_some()
    })
    .await;

    assert_eq!(harness.run(first.id).await.state, RunState::Active);
    assert_eq!(second.state, RunState::Active);

    harness.teardown().await;
}

#[tokio::test]
async fn the_active_run_limit_queues_excess_work_and_releases_it_as_runs_end() {
    let harness = Harness::dispatching_up_to(
        supervisor::binary(),
        &scripted_agent::playing(Script::Dawdles),
        1,
    )
    .await;
    let first_session = a_session(&harness).await;
    let second_session = harness.open_session("acme", "kestrel", "builder").await;
    let third_session = harness.open_session("acme", "kestrel", "builder").await;
    let first = harness.enqueue_run(first_session.id).await;
    let second = harness.enqueue_run(second_session.id).await;
    let third = harness.enqueue_run(third_session.id).await;

    let first = until(&harness, first.id, "started", |run| {
        run.started_at.is_some()
    })
    .await;
    assert_eq!(harness.run(second.id).await.state, RunState::Queued);
    assert_eq!(harness.run(third.id).await.state, RunState::Queued);

    harness.complete_run(&first).await;
    let second = until(&harness, second.id, "started", |run| {
        run.started_at.is_some()
    })
    .await;
    assert_eq!(harness.run(third.id).await.state, RunState::Queued);

    harness.complete_run(&second).await;
    let third = until(&harness, third.id, "started", |run| {
        run.started_at.is_some()
    })
    .await;
    assert_eq!(third.state, RunState::Active);

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_with_several_runs_in_flight_ends_each_and_stops_their_supervisors() {
    let environment = Environment::executing("sleep 300");
    let harness = Harness::dispatching_up_to(environment.path(), "unused", 2).await;
    let first_session = a_session(&harness).await;
    let second_session = harness.open_session("acme", "kestrel", "builder").await;
    let first = harness.enqueue_run(first_session.id).await;
    let second = harness.enqueue_run(second_session.id).await;
    let first = until(&harness, first.id, "reached a supervisor", |run| {
        run.supervisor.is_some()
    })
    .await;
    let second = until(&harness, second.id, "reached a supervisor", |run| {
        run.supervisor.is_some()
    })
    .await;

    let stopped = harness.teardown().await;

    for run in [first, second] {
        let ended = stopped.run(run.id).await;
        assert_eq!(ended.state, RunState::Ended);
        assert!(matches!(ended.exit, Some(Exit::Failed { .. })));
        Environment::named(run.supervisor.as_deref().expect("a supervisor"))
            .is_gone()
            .await;
    }
}

#[tokio::test]
async fn a_run_enqueued_is_claimed_dispatched_and_reaches_an_instance() {
    let harness = Harness::dispatching(supervisor::binary()).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    assert_eq!(run.state, RunState::Queued);
    let ended = ended(&harness, run.id).await;

    assert!(
        ended.connected.is_some(),
        "the run ended without a supervisor ever reaching the link"
    );
    assert!(ended.instance.is_some());
    assert_eq!(ended.exit, Some(Exit::Succeeded));

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_that_reaches_an_instance_starts_and_ends_in_the_transcript() {
    let harness = Harness::dispatching(supervisor::binary()).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    ended(&harness, run.id).await;

    let said: Vec<String> = harness
        .transcript(session.id)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect();

    assert_eq!(
        said,
        vec![
            "participant joined  builder".to_owned(),
            format!("run started  {}", run.id),
            "said  builder  half of one message, and the other half".to_owned(),
            "said  builder  a second message".to_owned(),
            format!("run ended  {}  succeeded", run.id),
        ]
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_finished_runs_supervisor_is_stopped_and_its_instance_kept_for_the_session() {
    let harness = Harness::dispatching(supervisor::binary()).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    Environment::named(ended.supervisor.as_deref().expect("a supervisor"))
        .is_gone()
        .await;
    let instance = ended.instance.expect("an instance");
    assert_eq!(harness.instance(session.id).await.as_ref(), Some(&instance));
    assert!(
        Environment::workspace_of(&instance).is_dir(),
        "the instance went with the run"
    );

    harness.teardown().await;
}

/// Stands in for the Agent Runtime, so what it finds is what the supervisor checked out
/// before spawning one, and then hands over to the agent it stands in for.
#[cfg(unix)]
fn noting_the_checkout() -> Environment {
    Environment::executing(&format!(
        "echo \"$(git -C kestrel branch --show-current) $(cat kestrel/README.md)\" \
           >> \"$(dirname \"$0\")/found\"\n\
         exec {}",
        scripted_agent::playing(Script::Speaks)
    ))
}

#[cfg(unix)]
async fn noted(runtime: &Environment, maximum: usize) -> Harness {
    Harness::dispatching_up_to(
        supervisor::binary(),
        &format!("\"{}\"", runtime.path().display()),
        maximum,
    )
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn a_session_declares_a_branch_of_its_own_and_the_run_starts_on_it() {
    let runtime = noting_the_checkout();
    let harness = noted(&runtime, 1).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert_eq!(session.checkout.branch, format!("kestrel/{}", session.id));
    assert_eq!(session.checkout.base, repository::BRANCH);
    assert_eq!(
        runtime.wrote("found"),
        format!("{} a workspace's repository", session.checkout.branch),
        "the agent did not start on its session's branch"
    );

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn parallel_sessions_work_on_distinct_branches() {
    let runtime = noting_the_checkout();
    let harness = noted(&runtime, 2).await;
    let first = a_session(&harness).await;
    let second = harness.open_session("acme", "kestrel", "builder").await;

    let runs = [
        harness.enqueue_run(first.id).await,
        harness.enqueue_run(second.id).await,
    ];
    for run in runs {
        ended(&harness, run.id).await;
    }

    assert_ne!(first.checkout.branch, second.checkout.branch);
    let mut found: Vec<String> = runtime.wrote("found").lines().map(str::to_owned).collect();
    found.sort();
    let mut expected = vec![
        format!("{} a workspace's repository", first.checkout.branch),
        format!("{} a workspace's repository", second.checkout.branch),
    ];
    expected.sort();
    assert_eq!(found, expected);

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_session_on_a_branch_its_operator_named_starts_on_that_branchs_work() {
    let runtime = noting_the_checkout();
    let harness = noted(&runtime, 1).await;
    a_session(&harness).await;
    let session = harness
        .open_session_on("acme", "kestrel", "builder", repository::EXISTING_BRANCH)
        .await;

    let run = harness.enqueue_run(session.id).await;
    ended(&harness, run.id).await;

    assert_eq!(
        runtime.wrote("found"),
        format!("{} an existing branch's work", repository::EXISTING_BRANCH)
    );

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_branch_the_remote_does_not_have_is_cut_from_the_workspaces() {
    let runtime = noting_the_checkout();
    let harness = noted(&runtime, 1).await;
    a_session(&harness).await;
    let session = harness
        .open_session_on("acme", "kestrel", "builder", "kestrel/issue-43")
        .await;

    let run = harness.enqueue_run(session.id).await;
    ended(&harness, run.id).await;

    assert_eq!(
        runtime.wrote("found"),
        "kestrel/issue-43 a workspace's repository"
    );

    harness.teardown().await;
}

/// Stands in for the Agent Runtime. On a checkout it has not been on before it leaves work only
/// this Instance has, and a process behind it; on one it has, it notes what it finds.
#[cfg(unix)]
fn leaving_work_behind() -> Environment {
    Environment::executing(&format!(
        "here=\"$(dirname \"$0\")\"\n\
         if [ -f kestrel/untracked ]; then\n\
           {{ git -C kestrel log -1 --format=%s; git -C kestrel status --porcelain; \
              cat kestrel/untracked; }} >> \"$here/found\"\n\
         else\n\
           echo fresh >> \"$here/found\"\n\
           {{ echo committed > kestrel/committed\n\
             git -C kestrel add committed\n\
             git -C kestrel -c user.name=kestrel -c user.email=kestrel@example.com \
               commit --message 'work only this instance has'\n\
             echo uncommitted >> kestrel/README.md\n\
             echo untracked > kestrel/untracked; }} >&2\n\
           sleep 300 </dev/null >/dev/null 2>&1 &\n\
           echo $! > \"$here/lingering\"\n\
         fi\n\
         exec {}",
        scripted_agent::playing(Script::Speaks)
    ))
}

/// The slot is held until the last Run's supervisor is stopped, a moment after the Run ends.
async fn enqueued_once_free(harness: &Harness, session: &Session) -> Run {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        match harness.try_enqueue_run(session.id).await {
            Ok(run) => return run,
            Err(error) => assert!(
                tokio::time::Instant::now() < deadline,
                "the session never took another run: {error}"
            ),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_later_run_finds_the_checkout_exactly_as_the_run_before_it_left_it() {
    let runtime = leaving_work_behind();
    let harness = noted(&runtime, 1).await;
    let session = a_session(&harness).await;

    let first = harness.enqueue_run(session.id).await;
    let first = ended(&harness, first.id).await;
    let second = enqueued_once_free(&harness, &session).await;
    let second = ended(&harness, second.id).await;

    assert_eq!(first.exit, Some(Exit::Succeeded));
    assert_eq!(second.exit, Some(Exit::Succeeded));
    assert_eq!(first.instance, second.instance);
    assert_eq!(
        runtime.wrote("found"),
        "fresh\n\
         work only this instance has\n \
         M README.md\n\
         ?? untracked\n\
         untracked"
    );

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn each_run_has_a_supervisor_of_its_own_and_leaves_no_process_to_the_next() {
    let runtime = leaving_work_behind();
    let harness = noted(&runtime, 1).await;
    let session = a_session(&harness).await;

    let first = harness.enqueue_run(session.id).await;
    let first = ended(&harness, first.id).await;
    Environment::process(&runtime.wrote("lingering"))
        .is_gone()
        .await;
    let first_supervisor = first.supervisor.expect("a supervisor");
    Environment::named(&first_supervisor).is_gone().await;

    let second = enqueued_once_free(&harness, &session).await;
    let second = ended(&harness, second.id).await;

    assert_eq!(first.instance, second.instance);
    assert_ne!(Some(first_supervisor), second.supervisor);

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_whose_instance_is_gone_fails_saying_so_and_the_next_starts_from_the_remote() {
    let runtime = leaving_work_behind();
    let harness = noted(&runtime, 1).await;
    let session = a_session(&harness).await;

    let first = harness.enqueue_run(session.id).await;
    let first = ended(&harness, first.id).await;
    Environment::named(first.supervisor.as_deref().expect("a supervisor"))
        .is_gone()
        .await;
    let lost = first.instance.clone().expect("an instance");
    std::fs::remove_dir_all(Environment::workspace_of(&lost)).expect("the instance should go");

    let second = enqueued_once_free(&harness, &session).await;
    let second = ended(&harness, second.id).await;
    let Some(Exit::Failed { because }) = &second.exit else {
        panic!("the run ended {:?}, and its instance was gone", second.exit);
    };
    assert!(
        because.contains(&lost)
            && because.contains("never pushed")
            && because.contains(&session.checkout.branch),
        "the failure does not say what was lost: {because}"
    );
    assert_eq!(second.started_at, None);
    assert_eq!(harness.instance(session.id).await, None);

    let third = enqueued_once_free(&harness, &session).await;
    let third = ended(&harness, third.id).await;
    assert_eq!(third.exit, Some(Exit::Succeeded));
    assert_ne!(third.instance.as_ref(), Some(&lost));
    assert_eq!(
        runtime.wrote("found"),
        "fresh\nfresh",
        "the run after a lost instance found work that was lost with it"
    );
    Environment::named(third.supervisor.as_deref().expect("a supervisor"))
        .is_gone()
        .await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_checkout_that_fails_names_the_repository_and_branch_and_the_run_never_starts() {
    let harness = Harness::dispatching(supervisor::binary()).await;
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &organization,
            repository::NAME,
            &[repository::url().to_owned()],
            "a-branch-nobody-cut",
        )
        .await;
    harness
        .declare_agent(
            &organization,
            "builder",
            "opencode",
            Some(kestrel_scripted_agent::OTHER_MODEL),
        )
        .await;
    harness
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;

    let session = harness.open_session("acme", "kestrel", "builder").await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and its workspace names a branch that is not there",
            ended.exit
        );
    };
    assert!(
        because.contains(repository::url()) && because.contains(&session.checkout.branch),
        "the failure names neither the repository nor the branch: {because}"
    );
    assert_eq!(
        ended.started_at, None,
        "a run that was never checked out started"
    );

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_supervisor_that_ends_without_saying_how_the_run_went_leaves_it_failed() {
    let environment = Environment::executing("exit 3");
    let harness = Harness::dispatching(environment.path()).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and its supervisor reported nothing",
            ended.exit
        );
    };
    assert!(
        because.contains("without reporting how the run went"),
        "unhelpful exit status: {because}"
    );
    Environment::named(ended.supervisor.as_deref().expect("a supervisor"))
        .is_gone()
        .await;

    harness.teardown().await;
}

#[tokio::test]
async fn a_supervisor_that_reports_its_run_failed_ends_it_failed() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, credential) = harness.dispatch_run(session.id).await;

    Link::to(&harness.link())
        .report(
            run.id,
            Some(&credential),
            &Reported {
                seq: Some(1),
                report: Report::Finished {
                    exit: Exit::Failed {
                        because: "the agent could not open a pull request".to_owned(),
                    },
                },
            },
        )
        .await;

    let ended = ended(&harness, run.id).await;
    assert_eq!(
        ended.exit,
        Some(Exit::Failed {
            because: "the agent could not open a pull request".to_owned()
        })
    );
    assert_eq!(
        harness
            .transcript(session.id)
            .await
            .last()
            .expect("a transcript entry")
            .entry
            .to_string(),
        format!(
            "run ended  {}  failed: the agent could not open a pull request",
            run.id
        )
    );

    harness.teardown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_still_in_flight_when_the_control_plane_stops_ends_and_its_supervisor_is_stopped() {
    let environment = Environment::executing("sleep 300");
    let harness = Harness::dispatching(environment.path()).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let in_flight = until(&harness, run.id, "reached a supervisor", |run| {
        run.supervisor.is_some() && run.state == RunState::Active
    })
    .await;

    let stopped = harness.teardown().await;

    let ended = stopped.run(run.id).await;
    assert_eq!(ended.state, RunState::Ended);
    assert!(matches!(ended.exit, Some(Exit::Failed { .. })));
    Environment::named(in_flight.supervisor.as_deref().expect("a supervisor"))
        .is_gone()
        .await;
}

#[tokio::test]
async fn a_run_whose_supervisor_cannot_be_started_ends_rather_than_staying_queued() {
    let harness = Harness::dispatching(Path::new("/nowhere/kestrel-supervisor")).await;
    let session = a_session(&harness).await;

    let run = harness.enqueue_run(session.id).await;
    let ended = ended(&harness, run.id).await;

    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!("the run ended {:?}, and nothing provisioned it", ended.exit);
    };
    assert!(
        because.contains("could not be started"),
        "unhelpful exit status: {because}"
    );
    assert!(ended.supervisor.is_none());

    harness.teardown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_run_is_claimed_once_however_many_claimants_ask_at_once() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let run = harness.enqueue_run(session.id).await;

    let (first, second) = tokio::join!(harness.claim_run(), harness.claim_run());

    let claimed: Vec<RunId> = [first, second]
        .into_iter()
        .flatten()
        .map(|claimed| claimed.run.id)
        .collect();
    assert_eq!(claimed, vec![run.id]);

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_being_executed_is_never_claimed_again() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;

    assert_eq!(harness.run(run.id).await.state, RunState::Active);
    assert!(
        harness.claim_run().await.is_none(),
        "a run already being executed was handed out to be dispatched again"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_run_that_ended_is_never_claimed_again() {
    let harness = Harness::boot().await;
    let session = a_session(&harness).await;
    let (run, _) = harness.dispatch_run(session.id).await;

    harness.complete_run(&run).await;

    assert!(
        harness.claim_run().await.is_none(),
        "a run that already ended was handed out to be dispatched again"
    );

    harness.teardown().await;
}
