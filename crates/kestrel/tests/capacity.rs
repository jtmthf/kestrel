mod support;

use jiff::{SignedDuration, Timestamp};
use kestrel::domain::{RunState, Workspace};
use kestrel::instance::{Git, Observed};
use support::Kestrel;
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

async fn workspaces(kestrel: &Kestrel, maximum: usize) -> (Workspace, Workspace, Workspace) {
    let organization = kestrel.declare_limited_organization("acme", maximum).await;
    kestrel
        .declare_project(
            &organization,
            repository::NAME,
            &[repository::url().to_owned()],
            repository::BRANCH,
        )
        .await;
    kestrel
        .declare_agent(
            &organization,
            "builder",
            "opencode",
            Some(kestrel_scripted_agent::OTHER_MODEL),
        )
        .await;
    kestrel
        .hold_provider_credential(
            &organization,
            support::PROVIDER_KEY,
            support::A_PROVIDER_KEY,
        )
        .await;

    (
        kestrel.open_workspace("acme", "kestrel", "builder").await,
        kestrel.open_workspace("acme", "kestrel", "builder").await,
        kestrel.open_workspace("acme", "kestrel", "builder").await,
    )
}

async fn complete_clean_runs(kestrel: &Kestrel, workspaces: &[(&Workspace, &str)]) {
    for (workspace, instance) in workspaces {
        let queued = kestrel.enqueue_run(workspace.id).await;
        let run = kestrel
            .occupy_run()
            .await
            .expect("the run should claim")
            .run;
        assert_eq!(run.id, queued.id);
        kestrel.executes_on(&run, instance).await;
        kestrel.report_checkout(&run, clean_checkout()).await;
        kestrel.complete_run(&run).await;
    }
}

#[tokio::test]
async fn an_active_instance_counts_toward_the_organization_limit() {
    let kestrel = Kestrel::boot().await;
    let (active, waiting, _) = workspaces(&kestrel, 1).await;
    let first = kestrel.enqueue_run(active.id).await;
    let claimed = kestrel.occupy_run().await.expect("the run should claim");
    assert_eq!(claimed.run.id, first.id);
    kestrel.executes_on(&claimed.run, "active").await;

    let second = kestrel.enqueue_run(waiting.id).await;
    assert!(kestrel.occupy_run().await.is_none());
    let second = kestrel.run(second.id).await;

    assert_eq!(second.state, RunState::Queued);
    assert!(second.waiting_for.is_some());

    kestrel.teardown().await;
}

#[tokio::test]
async fn reclaiming_for_new_work_does_not_delay_a_follow_up_that_already_has_an_instance() {
    let kestrel = Kestrel::boot().await;
    let (oldest, existing, arriving) = workspaces(&kestrel, 2).await;

    complete_clean_runs(&kestrel, &[(&oldest, "oldest"), (&existing, "existing")]).await;
    kestrel
        .last_active(&oldest, Timestamp::now() - SignedDuration::from_hours(1))
        .await;

    let new_run = kestrel.enqueue_run(arriving.id).await;
    let follow_up = kestrel.enqueue_run(existing.id).await;

    assert_eq!(
        kestrel.occupy_run().await.map(|claimed| claimed.run.id),
        Some(follow_up.id)
    );
    let new_run = kestrel.run(new_run.id).await;
    assert_eq!(new_run.state, RunState::Queued);
    assert!(new_run.waiting_for.is_some());
    assert_eq!(kestrel.instance(oldest.id).await, None);

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_held_instance_blocks_new_work_but_not_its_workspaces_follow_up() {
    let kestrel = Kestrel::boot().await;
    let (existing, new, _) = workspaces(&kestrel, 1).await;

    let first = kestrel.enqueue_run(existing.id).await;
    let claimed = kestrel.occupy_run().await.expect("the run should claim");
    assert_eq!(claimed.run.id, first.id);
    let first = claimed.run;
    kestrel.executes_on(&first, "held").await;
    let mut held = clean_checkout();
    held[0].git = Git::Read {
        branch: Some(repository::BRANCH.to_owned()),
        untracked: 0,
        uncommitted: 1,
        stashes: 0,
        unpushed: 0,
    };
    kestrel.report_checkout(&first, held).await;
    kestrel.complete_run(&first).await;

    let blocked = kestrel.enqueue_run(new.id).await;
    assert!(kestrel.occupy_run().await.is_none());
    let blocked = kestrel.run(blocked.id).await;
    assert_eq!(blocked.state, RunState::Queued);
    assert!(blocked.waiting_for.as_deref().is_some_and(|reason| {
        reason.contains("limit of 1 live Instance")
            && reason.contains("none idle is known recoverable")
    }));

    let follow_up = kestrel.enqueue_run(existing.id).await;
    let claimed = kestrel
        .occupy_run()
        .await
        .expect("the follow-up should claim");
    assert_eq!(claimed.run.id, follow_up.id);
    assert_eq!(kestrel.instance(existing.id).await.as_deref(), Some("held"));
    assert_eq!(kestrel.run(blocked.id).await.state, RunState::Queued);

    kestrel.teardown().await;
}

#[tokio::test]
async fn the_longest_idle_recoverable_instance_is_archived_to_admit_new_work() {
    let kestrel = Kestrel::boot().await;
    let (oldest, newer, arriving) = workspaces(&kestrel, 2).await;

    complete_clean_runs(&kestrel, &[(&oldest, "oldest"), (&newer, "newer")]).await;
    kestrel
        .last_active(&oldest, Timestamp::now() - SignedDuration::from_hours(2))
        .await;
    kestrel
        .last_active(&newer, Timestamp::now() - SignedDuration::from_hours(1))
        .await;

    let third = kestrel.enqueue_run(arriving.id).await;
    assert!(kestrel.occupy_run().await.is_none());

    assert_eq!(kestrel.instances_to_archive().await, ["oldest"]);
    assert_eq!(kestrel.instance(oldest.id).await, None);
    assert_eq!(kestrel.instance(newer.id).await.as_deref(), Some("newer"));

    kestrel.instance_archived("oldest").await;
    let claimed = kestrel
        .occupy_run()
        .await
        .expect("the new run should claim after archival");
    assert_eq!(claimed.run.id, third.id);

    kestrel.teardown().await;
}
