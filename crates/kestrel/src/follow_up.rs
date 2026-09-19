use anyhow::Result;

use crate::domain::{Event, EventRecordId, RunId, SessionId, SessionState};
use crate::integration::github;
use crate::store::Store;

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

/// A comment only feeds open work: a command belongs to the Triggers, and only one continues a sealed Session.
async fn receiving(store: &Store, event: &Event) -> Result<Received> {
    let mut tx = store.begin().await?;
    let mut session = tx
        .integrations()
        .session_for_follow_up(event)
        .await?
        .expect("an unfollowed event has an originating session");

    let holding = match (&session.state, &session.correlation) {
        (SessionState::Sealed, Some(correlation)) => {
            tx.sessions()
                .holding_correlation(&session.organization, correlation)
                .await?
        }
        _ => None,
    };
    if let Some(holding) = holding {
        session = tx.sessions().get(holding).await?;
    }

    let data = github::EventData::new(&event.occurrence);
    let run = if session.state == SessionState::Sealed || data.command().is_some() {
        None
    } else {
        crate::session::post_in(
            &mut tx,
            &session,
            data.actor().unwrap_or_default(),
            data.message().unwrap_or_default(),
        )
        .await?
    };
    tx.integrations().record_follow_up(event, &session).await?;
    tx.commit().await?;

    Ok(Received {
        event: event.record_id,
        session: session.id,
        run: run.map(|run| run.id),
    })
}
