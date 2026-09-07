use anyhow::{Result, bail};

use crate::domain::{Event, Organization, Run, RunState, Session, SessionId, SessionState};
use crate::fanout::{self, Change};
use crate::log::{Cursor, Entry, Page, Unreadable, Window};
use crate::store::{Store, Tx};

pub async fn open(
    store: &Store,
    organization: &str,
    workspace: &str,
    agent: &str,
    continues: Option<SessionId>,
) -> Result<Session> {
    let mut tx = store.begin().await?;

    let organization = tx.organization_named(organization).await?;
    let workspace = tx.workspace_named(&organization, workspace).await?;
    let agent = tx.agent_named(&organization, agent).await?;
    let continues = match continues {
        Some(sealed) => Some(continued(&mut tx, &organization, sealed).await?),
        None => None,
    };

    let session = tx
        .open_session(&organization, &workspace, &agent, continues.as_ref(), None)
        .await?;
    tx.log()
        .append(
            &session,
            Entry::ParticipantJoined {
                participant: session.agent.name.clone(),
            },
        )
        .await?;

    tx.commit().await?;
    fanout::publish(Change::SessionOpened(&session));

    Ok(session)
}

pub async fn seal(store: &Store, id: SessionId) -> Result<Session> {
    let mut tx = store.begin().await?;
    let session = tx.session(id).await?;

    if session.state == SessionState::Sealed {
        bail!("the session {id} is already sealed, and a sealed session is never reopened");
    }
    if let Some(holding) = tx.run_holding_the_slot(&session).await? {
        bail!("the run {holding} is still in flight in the session {id}");
    }

    let sealed_at = tx.seal_session(&session).await?;
    tx.commit().await?;

    let sealed = Session {
        state: SessionState::Sealed,
        sealed_at: Some(sealed_at),
        ..session
    };
    fanout::publish(Change::SessionSealed(&sealed));

    Ok(sealed)
}

pub async fn show(store: &Store, id: SessionId) -> Result<Session> {
    store.begin().await?.session(id).await
}

pub async fn sessions(store: &Store, organization: &str) -> Result<Vec<Session>> {
    let mut tx = store.begin().await?;
    let organization = tx.organization_named(organization).await?;

    tx.sessions(&organization).await
}

/// Read on its own rather than with the Session: most Sessions were opened by a person, and
/// every path that reports one would otherwise pay for the Event none of them has.
pub async fn started_by(store: &Store, session: &Session) -> Result<Option<Event>> {
    let Some(event) = session.started_by else {
        return Ok(None);
    };

    Ok(Some(store.begin().await?.event(event).await?))
}

pub async fn continuations(store: &Store, id: SessionId) -> Result<Vec<SessionId>> {
    store.begin().await?.continuations(id).await
}

pub async fn post(
    store: &Store,
    id: SessionId,
    participant: &str,
    message: &str,
) -> Result<Option<Run>> {
    let mut tx = store.begin().await?;
    let session = tx.session(id).await?;
    let run = post_in(&mut tx, &session, participant, message).await?;
    tx.commit().await?;

    Ok(run)
}

pub(crate) async fn post_in(
    tx: &mut Tx<'_>,
    session: &Session,
    participant: &str,
    message: &str,
) -> Result<Option<Run>> {
    session.accepts("message")?;
    tx.log()
        .append(
            session,
            Entry::Said {
                participant: participant.to_owned(),
                message: message.to_owned(),
            },
        )
        .await?;

    if let Some(holding) = tx.run_holding_the_slot(session).await? {
        if tx.run(holding).await?.state == RunState::Active {
            tx.mark_turn_pending(session).await?;
        }
        Ok(None)
    } else {
        Ok(Some(tx.enqueue_run(session).await?))
    }
}

pub async fn transcript(
    store: &Store,
    id: SessionId,
    from: Option<Cursor>,
    window: Window,
) -> Result<Page, Unreadable> {
    let mut tx = store.begin().await?;
    let session = tx.session(id).await?;

    tx.log().page(&session, from, window).await
}

/// Only a sealed Session is continued: work an open one could still take belongs in it.
async fn continued(tx: &mut Tx<'_>, organization: &Organization, id: SessionId) -> Result<Session> {
    let sealed = tx.session(id).await?;

    if sealed.organization.id != organization.id {
        bail!(
            "the session {id} belongs to the organization {}",
            sealed.organization.name
        );
    }
    if sealed.state != SessionState::Sealed {
        bail!("the session {id} is open, and work continues in it rather than after it");
    }

    Ok(sealed)
}
