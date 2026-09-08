//! No dependency edges, because everything queued at 0.1 is immediately eligible (ADR-0005).

use anyhow::{Context as _, Result, bail};
use jiff::{SignedDuration, Timestamp};

use crate::domain::{Exit, Run, RunId, SessionId, Usage};
use crate::integration::outcome;
use crate::link::credential::Secret;
use crate::log::{Entry, Message};
use crate::store::session::PendingMessage;
use crate::store::{Store, Tx};

/// Long enough that no Run outlives its own credential at 0.1, short enough that one left
/// behind by a control plane that died before ending its Run stops working on its own.
const CREDENTIAL_LIFETIME: SignedDuration = SignedDuration::from_hours(12);

/// An Environment cannot say it is alive while the control plane is not listening, so this
/// outlasts a restart under a live one by enough that an upgrade does not reap the Runs it
/// was carrying; a dead Environment holds a Session's active-Run slot until it is up.
const LEASE: SignedDuration = SignedDuration::from_mins(2);

/// The Secret is returned once, to be handed to the Environment at provision; `Store` keeps
/// only its digest, so it cannot be recovered afterwards.
pub struct Claimed {
    pub run: Run,
    pub credential: Secret,
}

pub async fn enqueue(store: &Store, session: SessionId) -> Result<Run> {
    let mut tx = store.begin().await?;
    let session = tx.sessions().get(session).await?;
    session.accepts("run")?;

    if let Some(holding) = tx.sessions().run_holding_the_slot(&session).await? {
        bail!(
            "the session {} already has the run {holding} in it, and a session has one at a time",
            session.id
        );
    }

    let run = tx.sessions().enqueue_run(&session).await?;
    tx.commit().await?;

    Ok(run)
}

/// A queued Run is dispatched at most once: what this hands back is already active, so a
/// second claimant asking at the same moment is handed something else, or nothing.
pub async fn claim(store: &Store) -> Result<Option<Claimed>> {
    let mut tx = store.begin().await?;
    let Some(run) = tx.sessions().claim_run(Timestamp::now() + LEASE).await? else {
        return Ok(None);
    };

    let credential = Secret::mint();
    tx.sessions()
        .issue_credential(
            &run,
            &credential.digest(),
            Timestamp::now() + CREDENTIAL_LIFETIME,
        )
        .await?;
    tx.commit().await?;

    Ok(Some(Claimed { run, credential }))
}

pub async fn run(store: &Store, id: RunId) -> Result<Run> {
    store.begin().await?.sessions().run(id).await
}

pub async fn runs(store: &Store, session: SessionId) -> Result<Vec<Run>> {
    let mut tx = store.begin().await?;
    let session = tx.sessions().get(session).await?;

    tx.sessions().runs(&session).await
}

pub async fn heartbeat(tx: &mut Tx<'_>, run: &Run) -> Result<()> {
    tx.sessions()
        .hold_lease(run, Timestamp::now() + LEASE)
        .await
}

pub async fn started(tx: &mut Tx<'_>, run: &Run) -> Result<()> {
    if tx.sessions().record_started(run).await? {
        let session = tx.sessions().get(run.session).await?;
        tx.log()
            .append(&session, Entry::RunStarted { run: run.id })
            .await?;
    }

    Ok(())
}

/// An Environment reports what its agent said; who said it is the Session's to know.
pub async fn said(tx: &mut Tx<'_>, run: &Run, message: &str) -> Result<()> {
    let session = tx.sessions().get(run.session).await?;
    tx.log()
        .append(
            &session,
            Entry::Said {
                participant: session.agent.name.clone(),
                message: message.to_owned(),
            },
        )
        .await?;

    Ok(())
}

/// On the Run, so what executed is on the record whether the Agent named it or the runtime
/// chose it; the models offered beside it are what a later declaration is refused against.
pub async fn on_the_model(
    tx: &mut Tx<'_>,
    run: &Run,
    model: &str,
    offered: &[String],
) -> Result<()> {
    let session = tx.sessions().get(run.session).await?;
    tx.sessions().record_model(run, model).await?;
    tx.agents()
        .record_models_advertised(session.organization.id, &session.agent.runtime, offered)
        .await
}

/// On the Run, and in no Transcript: what an agent spent is not a Session's shared state.
pub async fn used(tx: &mut Tx<'_>, run: &Run, usage: &Usage) -> Result<()> {
    tx.sessions().record_usage(run, usage).await
}

pub async fn provisioned(store: &Store, run: &Run, environment: &str) -> Result<()> {
    let mut tx = store.begin().await?;
    tx.sessions().record_environment(run, environment).await?;

    tx.commit().await
}

pub async fn environment_present(store: &Store, run: &Run, environment: &str) -> Result<()> {
    let mut tx = store.begin().await?;
    tx.sessions()
        .record_environment_present(run, environment)
        .await?;

    tx.commit().await
}

pub async fn environments_to_reap(store: &Store) -> Result<Vec<(Run, String)>> {
    store.begin().await?.sessions().environments_to_reap().await
}

pub async fn environment_gone(store: &Store, run: &Run) -> Result<Option<Run>> {
    let mut tx = store.begin().await?;
    tx.sessions().record_environment_gone(run).await?;
    let continued = continue_pending(&mut tx, run.session).await?;
    tx.commit().await?;

    Ok(continued)
}

pub async fn complete(store: &Store, run: &Run) -> Result<Exit> {
    end(store, run, Exit::Succeeded).await
}

pub async fn fail(store: &Store, run: &Run, because: &str) -> Result<Exit> {
    end(
        store,
        run,
        Exit::Failed {
            because: because.to_owned(),
        },
    )
    .await
}

async fn end(store: &Store, run: &Run, exit: Exit) -> Result<Exit> {
    let mut tx = store.begin().await?;
    let stands = ending(&mut tx, run, exit).await?;
    tx.commit().await?;

    Ok(stands)
}

/// A Run ends once. Whoever gets there first — the Environment reporting itself finished, the
/// claimant finding it gone, `timer` finding its lease expired — decides the exit status, and
/// what comes back is the one that stands.
pub(crate) async fn ending(tx: &mut Tx<'_>, run: &Run, exit: Exit) -> Result<Exit> {
    let stands = if tx.sessions().end_run(run, &exit).await? {
        let session = tx.sessions().get(run.session).await?;
        tx.log()
            .append(
                &session,
                Entry::RunEnded {
                    run: run.id,
                    exit: exit.clone(),
                },
            )
            .await?;
        tx.sessions().invalidate_credentials(run).await?;
        outcome::record(tx, run, &session, &exit).await?;
        if tx.sessions().environment_is_gone(run).await? {
            continue_pending(tx, run.session).await?;
        }
        exit
    } else {
        tx.sessions()
            .run(run.id)
            .await?
            .exit
            .context("a run that has ended has an exit status")?
    };

    Ok(stands)
}

async fn continue_pending(tx: &mut Tx<'_>, session: SessionId) -> Result<Option<Run>> {
    let session = tx.sessions().get(session).await?;
    if tx
        .sessions()
        .run_holding_the_slot(&session)
        .await?
        .is_some()
    {
        return Ok(None);
    }

    let pending = tx.sessions().take_pending_messages(&session).await?;
    if pending.is_empty() {
        return Ok(None);
    }

    tx.log().append(&session, pending_entry(pending)).await?;

    Ok(Some(tx.sessions().enqueue_run(&session).await?))
}

fn pending_entry(pending: Vec<PendingMessage>) -> Entry {
    Entry::Messages {
        messages: pending
            .into_iter()
            .map(|pending| Message {
                participant: pending.participant,
                message: pending.body,
            })
            .collect(),
    }
}
