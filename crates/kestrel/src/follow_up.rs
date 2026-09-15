use anyhow::Result;

use crate::domain::{Event, EventRecordId, RunId, SessionId, SessionState};
use crate::fanout::{self, Change};
use crate::integration::github;
use crate::log::Entry;
use crate::store::Store;
use crate::store::session::Opening;

const AT_A_TIME: usize = 32;

pub struct Received {
    pub event: EventRecordId,
    pub session: SessionId,
    pub run: Option<RunId>,
}

pub async fn receive(store: &Store) -> Result<Vec<Received>> {
    let events = {
        let mut tx = store.begin().await?;
        tx.integrations()
            .unfollowed(github::COMMENTED, AT_A_TIME)
            .await?
    };

    let mut received = Vec::with_capacity(events.len());
    for event in events {
        received.push(receiving(store, &event).await?);
    }

    Ok(received)
}

async fn receiving(store: &Store, event: &Event) -> Result<Received> {
    let mut tx = store.begin().await?;
    let mut session = tx
        .integrations()
        .session_for_follow_up(event)
        .await?
        .expect("an unfollowed event has an originating session");
    let mut opened = None;

    if session.state == SessionState::Sealed {
        let origin = tx
            .integrations()
            .event(
                session
                    .started_by
                    .expect("a triggered session has an event"),
            )
            .await?;
        // A follow-up is more of the same issue's work, so unlike a `--continues` opened by hand
        // it stays on the branch that work was pushed to.
        session = tx
            .sessions()
            .open(Opening {
                organization: &session.organization,
                workspace: &session.workspace,
                agent: &session.agent,
                branch: &session.branch,
                correlation: None,
                continues: Some(&session),
                started_by: Some(&origin),
            })
            .await?;
        tx.log()
            .append(
                &session,
                Entry::ParticipantJoined {
                    participant: session.agent.name.clone(),
                },
            )
            .await?;
        opened = Some(session.clone());
    }

    let data = github::EventData::new(&event.occurrence);
    let run = crate::session::post_in(
        &mut tx,
        &session,
        data.actor().unwrap_or_default(),
        data.message().unwrap_or_default(),
    )
    .await?;
    tx.integrations().record_follow_up(event, &session).await?;
    tx.commit().await?;

    if let Some(opened) = &opened {
        fanout::publish(Change::SessionOpened(opened));
    }

    Ok(Received {
        event: event.record_id,
        session: session.id,
        run: run.map(|run| run.id),
    })
}
