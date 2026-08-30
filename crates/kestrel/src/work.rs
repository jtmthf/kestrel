//! Enqueue, claim, heartbeat, complete, fail — and no dependency edges, because everything
//! queued at 0.1 is immediately eligible (ADR-0005).

use anyhow::Result;
use jiff::{SignedDuration, Timestamp};

use crate::domain::{Exit, Run, RunId, SessionId};
use crate::link::credential::Secret;
use crate::log::Entry;
use crate::store::Store;

/// Long enough that no Run outlives its own credential at 0.1, short enough that one left
/// behind by a control plane that died before ending its Run stops working on its own.
const CREDENTIAL_LIFETIME: SignedDuration = SignedDuration::from_hours(12);

/// The Secret is returned once, to be handed to the Environment at provision; `Store` keeps
/// only its digest, so it cannot be recovered afterwards.
pub struct Claimed {
    pub run: Run,
    pub credential: Secret,
}

pub async fn enqueue(store: &Store, session: SessionId) -> Result<Run> {
    let mut tx = store.begin().await?;
    let session = tx.session(session).await?;
    let run = tx.enqueue_run(&session).await?;
    tx.commit().await?;

    Ok(run)
}

/// A queued Run is dispatched at most once: what this hands back is already active, so a
/// second claimant asking at the same moment is handed something else, or nothing.
pub async fn claim(store: &Store) -> Result<Option<Claimed>> {
    let mut tx = store.begin().await?;
    let Some(run) = tx.claim_run().await? else {
        return Ok(None);
    };

    let credential = Secret::mint();
    tx.issue_credential(
        &run,
        &credential.digest(),
        Timestamp::now() + CREDENTIAL_LIFETIME,
    )
    .await?;
    tx.commit().await?;

    Ok(Some(Claimed { run, credential }))
}

pub async fn run(store: &Store, id: RunId) -> Result<Run> {
    store.begin().await?.run(id).await
}

pub async fn runs(store: &Store, session: SessionId) -> Result<Vec<Run>> {
    let mut tx = store.begin().await?;
    let session = tx.session(session).await?;

    tx.runs(&session).await
}

pub async fn heartbeat(store: &Store, run: &Run) -> Result<()> {
    let mut tx = store.begin().await?;
    tx.record_heartbeat(run).await?;

    tx.commit().await
}

pub async fn started(store: &Store, run: &Run) -> Result<()> {
    let mut tx = store.begin().await?;
    if tx.record_started(run).await? {
        let session = tx.session(run.session).await?;
        tx.log()
            .append(&session, Entry::RunStarted { run: run.id })
            .await?;
    }

    tx.commit().await
}

pub async fn provisioned(store: &Store, run: &Run, environment: &str) -> Result<()> {
    let mut tx = store.begin().await?;
    tx.record_environment(run, environment).await?;

    tx.commit().await
}

pub async fn complete(store: &Store, run: &Run) -> Result<()> {
    end(store, run, Exit::Succeeded).await
}

pub async fn fail(store: &Store, run: &Run, because: &str) -> Result<()> {
    end(
        store,
        run,
        Exit::Failed {
            because: because.to_owned(),
        },
    )
    .await
}

/// A Run ends once. Whoever gets there first — the Environment reporting itself finished, or
/// the claimant finding it gone — decides the exit status, and the other is a no-op.
async fn end(store: &Store, run: &Run, exit: Exit) -> Result<()> {
    let mut tx = store.begin().await?;
    if tx.end_run(run, &exit).await? {
        let session = tx.session(run.session).await?;
        tx.log()
            .append(&session, Entry::RunEnded { run: run.id, exit })
            .await?;
        tx.invalidate_credentials(run).await?;
    }

    tx.commit().await
}
