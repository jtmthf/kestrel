use anyhow::Result;

use crate::domain::{Session, SessionId};
use crate::fanout::{self, Change};
use crate::log::{Entry, TranscriptEntry};
use crate::store::Store;

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
