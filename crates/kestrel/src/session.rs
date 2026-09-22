use anyhow::{Result, bail};
use jiff::{SignedDuration, Timestamp};

use crate::domain::{Exit, Organization, Run, RunId, RunState, Session, SessionId, SessionState};
use crate::fanout::{self, Change};
use crate::instance;
use crate::log::{Cursor, Entry, Page, Unreadable, Window};
use crate::store::session::Opening;
use crate::store::{Store, Tx};
use crate::work;

/// Generous, because kestrel has no signal that a human is watching a Session: duration is
/// standing in for presence.
const IDLE: SignedDuration = SignedDuration::from_hours(24);

pub async fn open(
    store: &Store,
    organization: &str,
    workspace: &str,
    agent: &str,
    profile: Option<&str>,
    branch: Option<&str>,
    continues: Option<&str>,
) -> Result<Session> {
    let mut tx = store.begin().await?;

    let organization = tx.organizations().named(organization).await?;
    let workspace = tx.workspaces().named(&organization, workspace).await?;
    let agent = tx.agents().named(&organization, agent).await?;
    let profile = match profile {
        Some(profile) => Some(tx.profiles().named(&organization, profile).await?),
        None => None,
    };
    let continues = match continues {
        Some(reference) => Some(continued(&mut tx, &organization, reference).await?),
        None => None,
    };

    let session = tx
        .sessions()
        .open(Opening {
            organization: &organization,
            workspace: &workspace,
            agent: &agent,
            profile: profile.as_ref(),
            branch,
            correlation: None,
            continues: continues.as_ref(),
            started_by: None,
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

    tx.commit().await?;
    fanout::publish(Change::SessionOpened(&session));

    Ok(session)
}

pub async fn seal(store: &Store, id: SessionId) -> Result<Session> {
    let mut tx = store.begin().await?;
    let session = tx.sessions().get(id).await?;

    if session.state == SessionState::Sealed {
        bail!("the session {id} is already sealed, and a sealed session is never reopened");
    }
    if let Some(holding) = in_flight(&mut tx, &session).await? {
        bail!("the run {holding} is still in flight in the session {id}");
    }
    if let Some(waiting) = waiting(&mut tx, &session).await? {
        work::stopping(&mut tx, &waiting, Exit::Succeeded).await?;
    }
    instance::archive_on_seal(&mut tx, &session).await?;

    let sealed_at = tx.sessions().seal(&session).await?;
    tx.commit().await?;

    let sealed = Session {
        state: SessionState::Sealed,
        sealed_at: Some(sealed_at),
        ..session
    };
    fanout::publish(Change::SessionSealed(&sealed));

    Ok(sealed)
}

/// Unattended sealing, through the same command a person seals with, so nothing here can
/// decide differently to `seal`.
pub async fn seal_idle(store: &Store) -> Result<Vec<Session>> {
    let mut sealed = Vec::new();

    for id in idle(store).await? {
        sealed.push(seal(store, id).await?);
    }

    Ok(sealed)
}

async fn idle(store: &Store) -> Result<Vec<SessionId>> {
    let mut tx = store.begin().await?;
    let mut idle = Vec::new();

    for session in tx.sessions().idle(Timestamp::now() - IDLE).await? {
        let holds_unpublished_work = match tx.sessions().kept_instance(session.id).await? {
            Some(kept) => instance::unpublished(kept.observed.as_deref()).is_some(),
            None => false,
        };
        if !holds_unpublished_work && in_flight(&mut tx, &session).await?.is_none() {
            idle.push(session.id);
        }
    }

    Ok(idle)
}

/// What keeps a Session from sealing: a Run queued or mid-turn, or one that has ended while
/// messages are still waiting on it. A Run between turns is not: sealing ends it (ADR-0024).
pub(crate) async fn in_flight(tx: &mut Tx<'_>, session: &Session) -> Result<Option<RunId>> {
    let Some(holding) = tx.sessions().run_holding_the_slot(session).await? else {
        return Ok(None);
    };
    let run = tx.sessions().run(holding).await?;
    let still_going = (run.state != RunState::Ended && !tx.sessions().is_waiting(&run).await?)
        || tx.sessions().has_pending_messages(session).await?;

    Ok(still_going.then_some(holding))
}

async fn waiting(tx: &mut Tx<'_>, session: &Session) -> Result<Option<Run>> {
    let Some(holding) = tx.sessions().run_holding_the_slot(session).await? else {
        return Ok(None);
    };
    let run = tx.sessions().run(holding).await?;

    Ok(tx.sessions().is_waiting(&run).await?.then_some(run))
}

pub async fn show(store: &Store, id: SessionId) -> Result<Session> {
    store.begin().await?.sessions().get(id).await
}

pub async fn sessions(store: &Store, organization: &str) -> Result<Vec<Session>> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.sessions().all(&organization).await
}

pub async fn continuations(store: &Store, id: SessionId) -> Result<Vec<SessionId>> {
    store.begin().await?.sessions().continuations(id).await
}

pub async fn post(
    store: &Store,
    id: SessionId,
    participant: &str,
    message: &str,
) -> Result<Option<Run>> {
    let mut tx = store.begin().await?;
    let session = tx.sessions().get(id).await?;
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

    let holding = match tx.sessions().run_holding_the_slot(session).await? {
        Some(holding) => Some(tx.sessions().run(holding).await?),
        None => None,
    };
    match holding {
        None => {
            said(tx, session, participant, message).await?;
            Ok(Some(tx.sessions().enqueue_run(session, None).await?))
        }
        Some(run) if run.state == RunState::Queued => {
            said(tx, session, participant, message).await?;
            Ok(None)
        }
        // Held even for a Run between turns: its next turn waits for an active-work slot.
        Some(run) => {
            tx.sessions()
                .add_pending_message(session, participant, message)
                .await?;
            Ok(tx.sessions().is_waiting(&run).await?.then_some(run))
        }
    }
}

async fn said(tx: &mut Tx<'_>, session: &Session, participant: &str, message: &str) -> Result<()> {
    tx.log()
        .append(
            session,
            Entry::Said {
                participant: participant.to_owned(),
                message: message.to_owned(),
            },
        )
        .await?;

    Ok(())
}

pub async fn transcript(
    store: &Store,
    id: SessionId,
    from: Option<Cursor>,
    window: Window,
) -> Result<Page, Unreadable> {
    let mut tx = store.begin().await?;
    let session = tx.sessions().get(id).await?;

    tx.log().page(&session, from, window).await
}

pub async fn resolve(store: &Store, organization: &str, reference: &str) -> Result<Session> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.sessions().resolved(&organization, reference).await
}

/// Only a sealed Session is continued: work an open one could still take belongs in it.
async fn continued(
    tx: &mut Tx<'_>,
    organization: &Organization,
    reference: &str,
) -> Result<Session> {
    let sealed = tx.sessions().resolved(organization, reference).await?;

    if sealed.state != SessionState::Sealed {
        bail!(
            "the session {} is open, and work continues in it rather than after it",
            sealed.id
        );
    }

    Ok(sealed)
}
