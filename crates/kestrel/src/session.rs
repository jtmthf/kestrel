use anyhow::Result;
use jiff::{SignedDuration, Timestamp};

use crate::domain::{Run, RunId, Session, SessionId};
use crate::fanout::{self, Change};
use crate::link::credential::Secret;
use crate::log::{Entry, TranscriptEntry};
use crate::store::Store;

/// Long enough that no Run outlives its own credential at 0.1, short enough that one left
/// behind by a control plane that died before ending its Run stops working on its own.
const CREDENTIAL_LIFETIME: SignedDuration = SignedDuration::from_hours(12);

pub async fn open(
    store: &Store,
    organization: &str,
    workspace: &str,
    agent: &str,
) -> Result<Session> {
    let mut tx = store.begin().await?;

    let organization = tx.organization_named(organization).await?;
    let workspace = tx.workspace_named(&organization, workspace).await?;
    let agent = tx.agent_named(&organization, agent).await?;
    let session = tx.open_session(&organization, &workspace, &agent).await?;
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

pub async fn show(store: &Store, id: SessionId) -> Result<Session> {
    store.begin().await?.session(id).await
}

pub async fn transcript(store: &Store, id: SessionId) -> Result<Vec<TranscriptEntry>> {
    let mut tx = store.begin().await?;
    let session = tx.session(id).await?;

    tx.log().transcript(&session).await
}

/// The Secret is returned once, to be handed to the Environment at provision; `Store` keeps
/// only its digest, so it cannot be recovered afterwards.
pub async fn start_run(store: &Store, session: SessionId) -> Result<(Run, Secret)> {
    let mut tx = store.begin().await?;
    let session = tx.session(session).await?;
    let run = tx.start_run(&session).await?;

    let secret = Secret::mint();
    tx.issue_credential(
        &run,
        &secret.digest(),
        Timestamp::now() + CREDENTIAL_LIFETIME,
    )
    .await?;
    tx.commit().await?;

    Ok((run, secret))
}

pub async fn end_run(store: &Store, run: &Run) -> Result<()> {
    let mut tx = store.begin().await?;
    tx.end_run(run).await?;
    tx.invalidate_credentials(run).await?;

    tx.commit().await
}

pub async fn run(store: &Store, id: RunId) -> Result<Run> {
    store.begin().await?.run(id).await
}
