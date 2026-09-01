//! Ticket 02's durability assertions (tests/cli.rs), re-expressed against the primary test
//! seam instead of against the CLI subprocess.

mod support;

use kestrel::domain::{Run, Session, SessionState};
use kestrel::log::{Cursor, Unreadable, Window};
use support::Harness;

async fn declare_fixture(harness: &Harness) {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &organization,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    harness
        .declare_agent(&organization, "builder", "opencode", "claude-opus-5")
        .await;
}

#[tokio::test]
async fn the_harness_boots_a_complete_control_plane_against_a_fresh_database_and_tears_it_down() {
    let harness = Harness::boot().await;

    assert!(harness.data_dir().join("kestrel.db").exists());

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_opens_against_a_workspace_and_an_agent() {
    let harness = Harness::boot().await;
    declare_fixture(&harness).await;

    let session = harness.open_session("acme", "kestrel", "builder").await;
    let shown = harness.show_session(session.id).await;

    assert_eq!(shown.id, session.id);
    assert_eq!(shown.organization.name, "acme");
    assert_eq!(shown.workspace.name, "kestrel");
    assert_eq!(shown.agent.name, "builder");
    assert_eq!(shown.state, SessionState::Open);

    harness.teardown().await;
}

#[tokio::test]
async fn opening_a_session_records_the_agent_joining_it_as_its_first_transcript_entry() {
    let harness = Harness::boot().await;
    declare_fixture(&harness).await;

    let session = harness.open_session("acme", "kestrel", "builder").await;
    let transcript = harness.transcript(session.id).await;

    assert_eq!(transcript.len(), 1);
    assert_eq!(transcript[0].seq, 1);
    assert_eq!(
        transcript[0].entry.to_string(),
        "participant joined  builder"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn every_durable_record_carries_its_organization() {
    let harness = Harness::boot().await;
    declare_fixture(&harness).await;

    let session = harness.open_session("acme", "kestrel", "builder").await;
    let shown = harness.show_session(session.id).await;

    assert_eq!(shown.workspace.organization, shown.organization.id);
    assert_eq!(shown.agent.organization, shown.organization.id);

    harness.teardown().await;
}

#[tokio::test]
async fn declaring_a_workspace_and_an_agent_lists_them_back() {
    let harness = Harness::boot().await;
    let organization = harness.declare_organization("acme").await;

    let workspace = harness
        .declare_workspace(
            &organization,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    let agent = harness
        .declare_agent(&organization, "builder", "opencode", "claude-opus-5")
        .await;

    let workspaces = harness.workspaces(&organization).await;
    let agents = harness.agents(&organization).await;

    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].id, workspace.id);
    assert_eq!(workspaces[0].branch, "main");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, agent.id);
    assert_eq!(agents[0].runtime, "opencode");

    harness.teardown().await;
}

#[tokio::test]
async fn a_session_outlives_the_control_plane_being_killed_and_restarted() {
    let harness = Harness::boot().await;
    declare_fixture(&harness).await;
    let session = harness.open_session("acme", "kestrel", "builder").await;

    let before = harness.show_session(session.id).await;
    let before_transcript = harness.transcript(session.id).await;

    let harness = harness.kill_and_restart().await;

    let after = harness.show_session(session.id).await;
    let after_transcript = harness.transcript(session.id).await;

    assert_eq!(after.id, before.id);
    assert_eq!(after.state, before.state);
    assert_eq!(after.organization.name, before.organization.name);
    assert_eq!(
        after_transcript
            .iter()
            .map(|entry| entry.entry.to_string())
            .collect::<Vec<_>>(),
        before_transcript
            .iter()
            .map(|entry| entry.entry.to_string())
            .collect::<Vec<_>>()
    );

    harness.teardown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_harnesses_running_at_once_do_not_share_state() {
    let first = Harness::boot().await;
    let second = Harness::boot().await;

    // `join!` drives both concurrently, not one after the other, so a bug that leaked state
    // across harnesses (a shared file, a global) would show up as interleaved corruption.
    tokio::join!(
        first.declare_organization("acme"),
        second.declare_organization("globex"),
    );
    let (first_organizations, second_organizations) =
        tokio::join!(first.organizations(), second.organizations());

    assert_eq!(first_organizations.len(), 1);
    assert_eq!(first_organizations[0].name, "acme");
    assert_eq!(second_organizations.len(), 1);
    assert_eq!(second_organizations[0].name, "globex");

    first.teardown().await;
    second.teardown().await;
}

/// One entry for the Agent joining, and one for each thing it said.
async fn a_transcript_of(harness: &Harness, said: usize) -> (Session, Run) {
    declare_fixture(harness).await;
    let session = harness.open_session("acme", "kestrel", "builder").await;
    let (run, _) = harness.dispatch_run(session.id).await;

    for message in 1..=said {
        harness.said(&run, &format!("message {message}")).await;
    }

    (session, run)
}

fn two() -> Window {
    Window::of(2).expect("two entries is a window")
}

async fn walked(harness: &Harness, session: &Session, from: Option<Cursor>) -> Vec<i64> {
    harness
        .walk(session.id, from, two())
        .await
        .iter()
        .map(|entry| entry.seq)
        .collect()
}

#[tokio::test]
async fn a_read_returns_at_most_one_window_of_entries_and_a_cursor() {
    let harness = Harness::boot().await;
    let (session, _) = a_transcript_of(&harness, 4).await;

    let page = harness
        .page(session.id, None, two())
        .await
        .expect("the transcript should page");

    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[0].seq, 1);
    assert_eq!(page.entries[1].seq, 2);
    assert!(page.more);
    assert!(page.cursor.is_some());

    harness.teardown().await;
}

#[tokio::test]
async fn paging_walks_a_transcript_longer_than_one_window_with_no_gap_and_no_duplicate() {
    let harness = Harness::boot().await;
    let (session, _) = a_transcript_of(&harness, 6).await;

    assert_eq!(
        walked(&harness, &session, None).await,
        (1..=7).collect::<Vec<_>>()
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_cursor_still_walks_the_transcript_after_the_control_plane_restarts() {
    let harness = Harness::boot().await;
    let (session, _) = a_transcript_of(&harness, 3).await;
    let held = harness
        .page(session.id, None, two())
        .await
        .expect("the transcript should page")
        .cursor;

    let harness = harness.kill_and_restart().await;

    assert_eq!(walked(&harness, &session, held).await, vec![3, 4]);

    harness.teardown().await;
}

#[tokio::test]
async fn entries_appended_part_way_through_a_walk_land_after_what_was_already_walked() {
    let harness = Harness::boot().await;
    let (session, run) = a_transcript_of(&harness, 3).await;
    let held = harness
        .page(session.id, None, two())
        .await
        .expect("the transcript should page")
        .cursor;

    harness
        .said(&run, "said while the read was in flight")
        .await;

    assert_eq!(walked(&harness, &session, held).await, vec![3, 4, 5]);
    assert_eq!(
        harness.transcript(session.id).await[4].entry.to_string(),
        "said  builder  said while the read was in flight"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_cursor_that_walks_another_transcript_is_refused() {
    let harness = Harness::boot().await;
    let (session, _) = a_transcript_of(&harness, 3).await;
    let elsewhere = harness.open_session("acme", "kestrel", "builder").await;
    let held = harness
        .page(elsewhere.id, None, two())
        .await
        .expect("the transcript should page")
        .cursor;

    let refusal = harness.page(session.id, held, two()).await;

    assert!(
        matches!(refusal, Err(Unreadable::Cursor(_))),
        "a cursor from another transcript was taken"
    );

    harness.teardown().await;
}

#[tokio::test]
async fn a_cursor_naming_no_entry_is_refused_rather_than_restarting_the_walk() {
    let harness = Harness::boot().await;
    let (session, _) = a_transcript_of(&harness, 3).await;
    let nowhere: Cursor = format!("{}:99", session.id)
        .parse()
        .expect("a cursor is a session and a seq");

    let refusal = harness.page(session.id, Some(nowhere), two()).await;

    assert!(
        matches!(refusal, Err(Unreadable::Cursor(_))),
        "a cursor naming no entry was taken, and the walk started over"
    );

    harness.teardown().await;
}
