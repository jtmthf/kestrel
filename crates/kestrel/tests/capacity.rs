mod support;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{RunState, Session};
use kestrel::instance::{Git, Observed};
use support::Harness;
use support::repository;

fn clean_checkout() -> Vec<Observed> {
    vec![Observed {
        repository: repository::url().to_owned(),
        git: Git::Read {
            branch: Some(repository::BRANCH.to_owned()),
            untracked: 0,
            uncommitted: 0,
            stashes: 0,
            unpushed: 0,
        },
    }]
}

async fn sessions(harness: &Harness, maximum: usize) -> (Session, Session, Session) {
    let organization = harness.declare_limited_organization("acme", maximum).await;
    harness
        .declare_project(
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

    (
        harness.open_session("acme", "kestrel", "builder").await,
        harness.open_session("acme", "kestrel", "builder").await,
        harness.open_session("acme", "kestrel", "builder").await,
    )
}

async fn complete_clean_runs(harness: &Harness, sessions: &[(&Session, &str)]) {
    for (session, instance) in sessions {
        let queued = harness.enqueue_run(session.id).await;
        let run = harness
            .occupy_run()
            .await
            .expect("the run should claim")
            .run;
        assert_eq!(run.id, queued.id);
        harness.executes_on(&run, instance).await;
        harness.report_checkout(&run, clean_checkout()).await;
        harness.complete_run(&run).await;
    }
}

#[tokio::test]
async fn an_active_instance_counts_toward_the_organization_limit() {
    let harness = Harness::boot().await;
    let (active, waiting, _) = sessions(&harness, 1).await;
    let first = harness.enqueue_run(active.id).await;
    let claimed = harness.occupy_run().await.expect("the run should claim");
    assert_eq!(claimed.run.id, first.id);
    harness.executes_on(&claimed.run, "active").await;

    let second = harness.enqueue_run(waiting.id).await;
    assert!(harness.occupy_run().await.is_none());
    let second = harness.run(second.id).await;

    assert_eq!(second.state, RunState::Queued);
    assert!(second.waiting_for.is_some());

    harness.teardown().await;
}

#[tokio::test]
async fn reclaiming_for_new_work_does_not_delay_a_follow_up_that_already_has_an_instance() {
    let harness = Harness::boot().await;
    let (oldest, existing, arriving) = sessions(&harness, 2).await;

    complete_clean_runs(&harness, &[(&oldest, "oldest"), (&existing, "existing")]).await;
    harness
        .last_active(&oldest, Timestamp::now() - SignedDuration::from_hours(1))
        .await;

    let new_run = harness.enqueue_run(arriving.id).await;
    let follow_up = harness.enqueue_run(existing.id).await;

    assert_eq!(
        harness.occupy_run().await.map(|claimed| claimed.run.id),
        Some(follow_up.id)
    );
    let new_run = harness.run(new_run.id).await;
    assert_eq!(new_run.state, RunState::Queued);
    assert!(new_run.waiting_for.is_some());
    assert_eq!(harness.instance(oldest.id).await, None);

    harness.teardown().await;
}

#[tokio::test]
async fn a_held_instance_blocks_new_work_but_not_its_sessions_follow_up() {
    let harness = Harness::boot().await;
    let (existing, new, _) = sessions(&harness, 1).await;

    let first = harness.enqueue_run(existing.id).await;
    let claimed = harness.occupy_run().await.expect("the run should claim");
    assert_eq!(claimed.run.id, first.id);
    let first = claimed.run;
    harness.executes_on(&first, "held").await;
    let mut held = clean_checkout();
    held[0].git = Git::Read {
        branch: Some(repository::BRANCH.to_owned()),
        untracked: 0,
        uncommitted: 1,
        stashes: 0,
        unpushed: 0,
    };
    harness.report_checkout(&first, held).await;
    harness.complete_run(&first).await;

    let blocked = harness.enqueue_run(new.id).await;
    assert!(harness.occupy_run().await.is_none());
    let blocked = harness.run(blocked.id).await;
    assert_eq!(blocked.state, RunState::Queued);
    assert!(blocked.waiting_for.as_deref().is_some_and(|reason| {
        reason.contains("limit of 1 live Instance")
            && reason.contains("none idle is known recoverable")
    }));

    let follow_up = harness.enqueue_run(existing.id).await;
    let claimed = harness
        .occupy_run()
        .await
        .expect("the follow-up should claim");
    assert_eq!(claimed.run.id, follow_up.id);
    assert_eq!(harness.instance(existing.id).await.as_deref(), Some("held"));
    assert_eq!(harness.run(blocked.id).await.state, RunState::Queued);

    harness.teardown().await;
}

#[tokio::test]
async fn the_longest_idle_recoverable_instance_is_archived_to_admit_new_work() {
    let harness = Harness::boot().await;
    let (oldest, newer, arriving) = sessions(&harness, 2).await;

    complete_clean_runs(&harness, &[(&oldest, "oldest"), (&newer, "newer")]).await;
    harness
        .last_active(&oldest, Timestamp::now() - SignedDuration::from_hours(2))
        .await;
    harness
        .last_active(&newer, Timestamp::now() - SignedDuration::from_hours(1))
        .await;

    let third = harness.enqueue_run(arriving.id).await;
    assert!(harness.occupy_run().await.is_none());

    assert_eq!(harness.instances_to_archive().await, ["oldest"]);
    assert_eq!(harness.instance(oldest.id).await, None);
    assert_eq!(harness.instance(newer.id).await.as_deref(), Some("newer"));

    harness.instance_archived("oldest").await;
    let claimed = harness
        .occupy_run()
        .await
        .expect("the new run should claim after archival");
    assert_eq!(claimed.run.id, third.id);

    harness.teardown().await;
}
