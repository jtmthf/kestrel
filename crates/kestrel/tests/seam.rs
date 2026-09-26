//! Ticket 02's durability assertions (tests/cli.rs), re-expressed against the primary test
//! seam instead of against the CLI subprocess.

mod support;

use kestrel::domain::{Run, Session, SessionState};
use kestrel::log::{Cursor, Unreadable, Window};
use support::Kestrel;

async fn declare_fixture(kestrel: &Kestrel) {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(
            &organization,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    kestrel
        .declare_agent(&organization, "builder", "opencode", Some("claude-opus-5"))
        .await;
}

#[tokio::test]
async fn the_fixture_boots_a_complete_control_plane_against_a_fresh_database_and_tears_it_down() {
    let kestrel = Kestrel::boot().await;

    assert!(kestrel.data_dir().join("kestrel.db").exists());

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_session_opens_against_a_project_and_an_agent() {
    let kestrel = Kestrel::boot().await;
    declare_fixture(&kestrel).await;

    let session = kestrel.open_session("acme", "kestrel", "builder").await;
    let shown = kestrel.show_session(session.id).await;

    assert_eq!(shown.id, session.id);
    assert_eq!(shown.organization.name, "acme");
    assert_eq!(shown.project.name, "kestrel");
    assert_eq!(shown.agent.name, "builder");
    assert_eq!(shown.state, SessionState::Open);

    kestrel.teardown().await;
}

#[tokio::test]
async fn opening_a_session_records_the_agent_joining_it_as_its_first_transcript_entry() {
    let kestrel = Kestrel::boot().await;
    declare_fixture(&kestrel).await;

    let session = kestrel.open_session("acme", "kestrel", "builder").await;
    let transcript = kestrel.transcript(session.id).await;

    assert_eq!(transcript.len(), 1);
    assert_eq!(transcript[0].seq, 1);
    assert_eq!(
        transcript[0].entry.to_string(),
        "participant joined  builder"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn every_durable_record_carries_its_organization() {
    let kestrel = Kestrel::boot().await;
    declare_fixture(&kestrel).await;

    let session = kestrel.open_session("acme", "kestrel", "builder").await;
    let shown = kestrel.show_session(session.id).await;

    assert_eq!(shown.project.organization, shown.organization.id);
    assert_eq!(shown.agent.organization, shown.organization.id);

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_project_redeclared_after_a_session_opens_moves_none_of_its_checkout() {
    let kestrel = Kestrel::boot().await;
    declare_fixture(&kestrel).await;
    let session = kestrel.open_session("acme", "kestrel", "builder").await;
    let organization = kestrel.declare_organization("acme").await;

    kestrel
        .declare_project(
            &organization,
            "kestrel",
            &["https://github.com/jtmthf/elsewhere".to_owned()],
            "trunk",
        )
        .await;

    let shown = kestrel.show_session(session.id).await;
    assert_eq!(
        shown.checkout.repositories,
        vec!["https://github.com/jtmthf/kestrel".to_owned()]
    );
    assert_eq!(shown.checkout.base, "main");
    assert_eq!(shown.checkout, session.checkout);

    kestrel.teardown().await;
}

#[tokio::test]
async fn declaring_a_project_and_an_agent_lists_them_back() {
    let kestrel = Kestrel::boot().await;
    let organization = kestrel.declare_organization("acme").await;

    let project = kestrel
        .declare_project(
            &organization,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    let agent = kestrel
        .declare_agent(&organization, "builder", "opencode", Some("claude-opus-5"))
        .await;

    let projects = kestrel.projects(&organization).await;
    let agents = kestrel.agents(&organization).await;

    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, project.id);
    assert_eq!(projects[0].branch, "main");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, agent.id);
    assert_eq!(agents[0].runtime, "opencode");

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_session_outlives_the_control_plane_being_killed_and_restarted() {
    let kestrel = Kestrel::boot().await;
    declare_fixture(&kestrel).await;
    let session = kestrel.open_session("acme", "kestrel", "builder").await;

    let before = kestrel.show_session(session.id).await;
    let before_transcript = kestrel.transcript(session.id).await;

    let kestrel = kestrel.kill_and_restart().await;

    let after = kestrel.show_session(session.id).await;
    let after_transcript = kestrel.transcript(session.id).await;

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

    kestrel.teardown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_kestrels_running_at_once_do_not_share_state() {
    let first = Kestrel::boot().await;
    let second = Kestrel::boot().await;

    // `join!` drives both concurrently, not one after the other, so a bug that leaked state
    // across kestrels (a shared file, a global) would show up as interleaved corruption.
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
async fn a_transcript_of(kestrel: &Kestrel, said: usize) -> (Session, Run) {
    declare_fixture(kestrel).await;
    let session = kestrel.open_session("acme", "kestrel", "builder").await;
    let (run, _) = kestrel.dispatch_run(session.id).await;

    for message in 1..=said {
        kestrel.said(&run, &format!("message {message}")).await;
    }

    (session, run)
}

fn two() -> Window {
    Window::of(2).expect("two entries is a window")
}

async fn walked(kestrel: &Kestrel, session: &Session, from: Option<Cursor>) -> Vec<i64> {
    kestrel
        .walk(session.id, from, two())
        .await
        .iter()
        .map(|entry| entry.seq)
        .collect()
}

#[tokio::test]
async fn a_read_returns_at_most_one_window_of_entries_and_a_cursor() {
    let kestrel = Kestrel::boot().await;
    let (session, _) = a_transcript_of(&kestrel, 4).await;

    let page = kestrel
        .page(session.id, None, two())
        .await
        .expect("the transcript should page");

    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[0].seq, 1);
    assert_eq!(page.entries[1].seq, 2);
    assert!(page.more);
    assert!(page.cursor.is_some());

    kestrel.teardown().await;
}

#[tokio::test]
async fn paging_walks_a_transcript_longer_than_one_window_with_no_gap_and_no_duplicate() {
    let kestrel = Kestrel::boot().await;
    let (session, _) = a_transcript_of(&kestrel, 6).await;

    assert_eq!(
        walked(&kestrel, &session, None).await,
        (1..=7).collect::<Vec<_>>()
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_cursor_still_walks_the_transcript_after_the_control_plane_restarts() {
    let kestrel = Kestrel::boot().await;
    let (session, _) = a_transcript_of(&kestrel, 3).await;
    let held = kestrel
        .page(session.id, None, two())
        .await
        .expect("the transcript should page")
        .cursor;

    let kestrel = kestrel.kill_and_restart().await;

    assert_eq!(walked(&kestrel, &session, held).await, vec![3, 4]);

    kestrel.teardown().await;
}

#[tokio::test]
async fn entries_appended_part_way_through_a_walk_land_after_what_was_already_walked() {
    let kestrel = Kestrel::boot().await;
    let (session, run) = a_transcript_of(&kestrel, 3).await;
    let held = kestrel
        .page(session.id, None, two())
        .await
        .expect("the transcript should page")
        .cursor;

    kestrel
        .said(&run, "said while the read was in flight")
        .await;

    assert_eq!(walked(&kestrel, &session, held).await, vec![3, 4, 5]);
    assert_eq!(
        kestrel.transcript(session.id).await[4].entry.to_string(),
        "said  builder  said while the read was in flight"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_cursor_that_walks_another_transcript_is_refused() {
    let kestrel = Kestrel::boot().await;
    let (session, _) = a_transcript_of(&kestrel, 3).await;
    let elsewhere = kestrel.open_session("acme", "kestrel", "builder").await;
    let held = kestrel
        .page(elsewhere.id, None, two())
        .await
        .expect("the transcript should page")
        .cursor;

    let refusal = kestrel.page(session.id, held, two()).await;

    assert!(
        matches!(refusal, Err(Unreadable::Cursor(_))),
        "a cursor from another transcript was taken"
    );

    kestrel.teardown().await;
}

#[tokio::test]
async fn a_cursor_naming_no_entry_is_refused_rather_than_restarting_the_walk() {
    let kestrel = Kestrel::boot().await;
    let (session, _) = a_transcript_of(&kestrel, 3).await;
    let nowhere: Cursor = format!("{}:99", session.id)
        .parse()
        .expect("a cursor is a session and a seq");

    let refusal = kestrel.page(session.id, Some(nowhere), two()).await;

    assert!(
        matches!(refusal, Err(Unreadable::Cursor(_))),
        "a cursor naming no entry was taken, and the walk started over"
    );

    kestrel.teardown().await;
}
