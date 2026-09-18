use std::fmt;

use anyhow::{Context as _, Result, bail};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::domain::{Exit, Run, RunId, SessionId, Usage};
use crate::instance::Observed;
use crate::integration::outcome;
use crate::link::credential::Secret;
use crate::log::{Entry, Message};
use crate::store::session::{PendingMessage, Taken};
use crate::store::{Store, Tx};

const CREDENTIAL_LIFETIME: SignedDuration = SignedDuration::from_hours(12);

/// A supervisor cannot say it is alive while the control plane is not listening, so this
/// outlasts a restart under a live one by enough that an upgrade does not reap the Runs it
/// was carrying; a dead supervisor holds a Session's active-Run slot until it is up.
const LEASE: SignedDuration = SignedDuration::from_mins(2);

/// The Secret is returned once, to be handed to the Run's supervisor as it starts; `Store` keeps
/// only its digest, so it cannot be recovered afterwards.
pub struct Claimed {
    pub run: Run,
    pub credential: Secret,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Report {
    Connected { version: String },
    Heartbeat,
    Started,
    Model { model: String, offered: Vec<String> },
    Said { message: String },
    Used { usage: Usage },
    Checkout { repositories: Vec<Observed> },
    Finished { exit: Exit },
}

impl Report {
    const fn numbered(&self) -> bool {
        match self {
            Report::Connected { .. } | Report::Heartbeat => false,
            Report::Started
            | Report::Model { .. }
            | Report::Said { .. }
            | Report::Used { .. }
            | Report::Checkout { .. }
            | Report::Finished { .. } => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reported {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    #[serde(flatten)]
    pub report: Report,
}

#[derive(Debug)]
pub enum ReportRefused {
    MissingSequence,
    SkippedSequence(i64),
    Unavailable(anyhow::Error),
}

impl fmt::Display for ReportRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSequence => {
                write!(
                    f,
                    "a report of this kind carries a seq, and this one carries none"
                )
            }
            Self::SkippedSequence(seq) => {
                write!(f, "the report {seq} skips one this run has yet to report")
            }
            Self::Unavailable(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ReportRefused {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unavailable(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl From<anyhow::Error> for ReportRefused {
    fn from(error: anyhow::Error) -> Self {
        Self::Unavailable(error)
    }
}

pub async fn enqueue(store: &Store, session: SessionId, model: Option<&str>) -> Result<Run> {
    let mut tx = store.begin().await?;
    let session = tx.sessions().get(session).await?;
    session.accepts("run")?;

    if let Some(holding) = tx.sessions().run_holding_the_slot(&session).await? {
        bail!(
            "the session {} already has the run {holding} in it, and a session has one at a time",
            session.id
        );
    }

    let run = tx.sessions().enqueue_run(&session, model).await?;
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

/// Report acceptance and its effects share one transaction so a failed append remains replayable (ADR-0004).
pub async fn report(
    store: &Store,
    run: &Run,
    Reported { seq, report }: Reported,
) -> Result<(), ReportRefused> {
    let mut tx = store.begin().await?;

    if report.numbered() {
        let seq = seq.ok_or(ReportRefused::MissingSequence)?;
        match tx.sessions().take_report(run, seq).await? {
            Taken::Next => {}
            Taken::Again => {
                debug!(run = %run.id, seq, "a supervisor reported something again");
                return Ok(());
            }
            Taken::Skipped => return Err(ReportRefused::SkippedSequence(seq)),
        }
    }

    match report {
        Report::Connected { version } => {
            tx.sessions().record_connected(run, &version).await?;
            info!(run = %run.id, version, "a supervisor reported itself connected");
        }
        Report::Heartbeat => {
            tx.sessions()
                .hold_lease(run, Timestamp::now() + LEASE)
                .await?;
            debug!(run = %run.id, "a supervisor reported itself alive");
        }
        Report::Started => {
            if tx.sessions().record_started(run).await? {
                let session = tx.sessions().get(run.session).await?;
                tx.log()
                    .append(&session, Entry::RunStarted { run: run.id })
                    .await?;
            }
            info!(run = %run.id, "a supervisor reported its run started");
        }
        Report::Model { model, offered } => {
            let session = tx.sessions().get(run.session).await?;
            tx.sessions().record_worked_model(run, &model).await?;
            tx.agents()
                .record_models_advertised(session.organization.id, &session.agent.runtime, &offered)
                .await?;
            info!(run = %run.id, model, "a supervisor reported the model its agent is on");
        }
        Report::Said { message } => {
            let session = tx.sessions().get(run.session).await?;
            tx.log()
                .append(
                    &session,
                    Entry::Said {
                        participant: session.agent.name.clone(),
                        message,
                    },
                )
                .await?;
            info!(run = %run.id, "a supervisor reported what its agent said");
        }
        Report::Used { usage } => {
            info!(run = %run.id, %usage, "a supervisor reported what its agent used");
            tx.sessions().record_usage(run, &usage).await?;
        }
        Report::Checkout { repositories } => {
            tx.sessions()
                .record_observed(run.session, &repositories)
                .await?;
            info!(run = %run.id, "a supervisor reported what its checkout holds");
        }
        Report::Finished { exit } => {
            let stands = ending(&mut tx, run, exit).await?;
            info!(run = %run.id, %stands, "a supervisor reported its run finished");
        }
    }
    tx.commit().await?;

    Ok(())
}

pub async fn instance(store: &Store, session: SessionId) -> Result<Option<String>> {
    store.begin().await?.sessions().instance(session).await
}

pub async fn executes_on(store: &Store, run: &Run, instance: &str) -> Result<()> {
    let mut tx = store.begin().await?;
    tx.sessions()
        .record_instance(run.session, Some(instance))
        .await?;
    tx.sessions().record_run_instance(run, instance).await?;

    tx.commit().await
}

/// Forgotten in the same breath as the Run fails, so no later Run is handed an Instance nothing
/// can resume, and none is handed a fresh one before this Run says what was lost.
pub async fn instance_lost(store: &Store, run: &Run, because: &str) -> Result<Exit> {
    let mut tx = store.begin().await?;
    tx.sessions().record_instance(run.session, None).await?;
    let stands = ending(
        &mut tx,
        run,
        Exit::Failed {
            because: because.to_owned(),
        },
    )
    .await?;
    tx.commit().await?;

    Ok(stands)
}

pub async fn supervised(store: &Store, run: &Run, supervisor: &str) -> Result<()> {
    let mut tx = store.begin().await?;
    tx.sessions().record_supervisor(run, supervisor).await?;

    tx.commit().await
}

pub async fn supervisors_to_stop(store: &Store) -> Result<Vec<(Run, String)>> {
    store.begin().await?.sessions().supervisors_to_stop().await
}

pub async fn supervisor_gone(store: &Store, run: &Run) -> Result<Option<Run>> {
    let mut tx = store.begin().await?;
    tx.sessions().record_supervisor_gone(run).await?;
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

/// A Run ends once. Whoever gets there first — the supervisor reporting itself finished, the
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
        if let Exit::Failed { .. } = exit {
            cascade_unreachable(tx, run.id).await?;
        }
        if tx.sessions().supervisor_is_gone(run).await? {
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

/// Tolerance defaults to all-must-succeed, so a Run that failed makes every Run still queued
/// on it unreachable — and whatever was in turn waiting on one of those, since a blocker that
/// will never succeed cannot meet an all-must-succeed tolerance either. Never touches a Run
/// a claimant already took past this blocker before it failed.
async fn cascade_unreachable(tx: &mut Tx<'_>, blocker: RunId) -> Result<()> {
    let mut newly_unreachable = vec![blocker];

    while let Some(blocker) = newly_unreachable.pop() {
        for dependent in tx.sessions().dependents_of(blocker).await? {
            if tx.sessions().mark_unreachable(&dependent).await? {
                newly_unreachable.push(dependent.id);
            }
        }
    }

    Ok(())
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

    Ok(Some(tx.sessions().enqueue_run(&session, None).await?))
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

#[cfg(test)]
mod tests;
