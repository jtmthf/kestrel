use std::fmt;

use anyhow::{Context as _, Result, bail};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::domain::{Exit, Run, RunId, RunState, SessionId, Turn, Usage};
use crate::instance::Observed;
use crate::integration::delivery;
use crate::link;
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
    Stderr { lines: Vec<String> },
    Started,
    Model { model: String, offered: Vec<String> },
    Said { message: String },
    Used { usage: Usage },
    Answered,
    Checkout { repositories: Vec<Observed> },
    Finished { exit: Exit },
}

impl Report {
    const fn numbered(&self) -> bool {
        match self {
            Report::Connected { .. } | Report::Heartbeat | Report::Stderr { .. } => false,
            Report::Started
            | Report::Model { .. }
            | Report::Said { .. }
            | Report::Used { .. }
            | Report::Answered
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
pub async fn claim(store: &Store, serialized: &[String]) -> Result<Option<Claimed>> {
    let mut tx = store.begin().await?;
    let claimed = claiming(&mut tx, serialized, None).await?;
    tx.commit().await?;

    Ok(claimed)
}

pub enum Occupied {
    Claimed(Claimed),
    Resumed(Run),
}

/// A Run between turns holds no slot (ADR-0024), so a free one goes to whichever asked for it
/// first: a queued Run, or input held for a Run between turns.
pub async fn occupy(
    store: &Store,
    slots: usize,
    serialized: &[String],
) -> Result<Option<Occupied>> {
    let mut tx = store.begin().await?;
    if tx.sessions().occupying_slots().await? >= slots {
        return Ok(None);
    }

    let held = tx.sessions().oldest_held_input(serialized).await?;
    let occupied =
        match claiming(&mut tx, serialized, held.as_ref().map(|(_, since)| *since)).await? {
            Some(claimed) => Occupied::Claimed(claimed),
            None => {
                let Some((run, _)) = held else {
                    tx.commit().await?;
                    return Ok(None);
                };
                prompt_pending(&mut tx, &run).await?;
                Occupied::Resumed(run)
            }
        };
    tx.commit().await?;

    Ok(Some(occupied))
}

async fn claiming(
    tx: &mut Tx<'_>,
    serialized: &[String],
    enqueued_before: Option<Timestamp>,
) -> Result<Option<Claimed>> {
    let claimable = tx
        .sessions()
        .claimable_runs(serialized, enqueued_before)
        .await?;
    for queued in claimable {
        let session = tx.sessions().get(queued.session).await?;
        match crate::instance::admit(tx, &session).await? {
            crate::instance::Admission::Available => {
                let Some(run) = tx
                    .sessions()
                    .claim_run(&queued, Timestamp::now() + LEASE)
                    .await?
                else {
                    continue;
                };
                let credential = Secret::mint();
                tx.sessions()
                    .issue_credential(
                        &run,
                        &credential.digest(),
                        Timestamp::now() + CREDENTIAL_LIFETIME,
                    )
                    .await?;
                return Ok(Some(Claimed { run, credential }));
            }
            crate::instance::Admission::Waiting(because) => {
                tx.sessions().wait_for_instance(&queued, &because).await?;
            }
        }
    }

    Ok(None)
}

pub async fn run(store: &Store, id: RunId) -> Result<Run> {
    store.begin().await?.sessions().run(id).await
}

pub async fn resolve_run(store: &Store, organization: &str, reference: &str) -> Result<Run> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.sessions().resolved_run(&organization, reference).await
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
        Report::Stderr { lines } => {
            for line in lines {
                info!(run = %run.id, line, "its agent runtime wrote to stderr");
            }
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
        Report::Answered => {
            if let Some((turn, from_seq)) = tx.sessions().answer_turn(run).await? {
                let session = tx.sessions().get(run.session).await?;
                let said = tx
                    .log()
                    .said_since(&session, from_seq, &session.agent.name)
                    .await?;
                if !said.is_empty() {
                    delivery::record_turn(&mut tx, run, &session, turn, &said).await?;
                }
                tx.sessions()
                    .record_active(run.session, Timestamp::now())
                    .await?;
            }
            info!(run = %run.id, "a supervisor reported its agent answered a turn");
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

pub async fn is_waiting(store: &Store, run: &Run) -> Result<bool> {
    store.begin().await?.sessions().is_waiting(run).await
}

pub async fn turns(store: &Store, run: RunId) -> Result<Vec<Turn>> {
    store.begin().await?.sessions().turns(run).await
}

/// A Run between turns has done everything asked of it, so stopping it there is how it
/// succeeds; stopping one mid-turn abandons what its agent was still doing.
pub async fn stop(store: &Store, id: RunId) -> Result<Exit> {
    let mut tx = store.begin().await?;
    let run = tx.sessions().run(id).await?;
    let exit = match run.state {
        RunState::Ended | RunState::Unreachable => bail!("the run {id} has already ended"),
        RunState::Queued => failed("it was stopped before it started"),
        RunState::Active if tx.sessions().is_waiting(&run).await? => Exit::Succeeded,
        RunState::Active => failed("it was stopped mid-turn, before its agent answered"),
    };
    let stands = stopping(&mut tx, &run, exit).await?;
    tx.commit().await?;

    Ok(stands)
}

/// Told as well as ended, so a supervisor leaves the link rather than dialling back in to be
/// refused.
pub(crate) async fn stopping(tx: &mut Tx<'_>, run: &Run, exit: Exit) -> Result<Exit> {
    tx.sessions()
        .send_instruction(run, link::Instruction::Stop)
        .await?;

    ending(tx, run, exit).await
}

fn failed(because: &str) -> Exit {
    Exit::Failed {
        because: because.to_owned(),
    }
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
    let session = tx.sessions().get(run.session).await?;
    let said = tx.log().last_said_for_run(&session).await?;
    let stands = if tx.sessions().end_run(run, &exit, said.as_deref()).await? {
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
        delivery::record_outcome(tx, run, &session, &exit, said.as_deref()).await?;
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

    tx.log()
        .append(
            &session,
            Entry::Messages {
                messages: messages(pending),
            },
        )
        .await?;

    Ok(Some(tx.sessions().enqueue_run(&session, None).await?))
}

async fn prompt_pending(tx: &mut Tx<'_>, run: &Run) -> Result<()> {
    let session = tx.sessions().get(run.session).await?;
    let pending = tx.sessions().take_pending_messages(&session).await?;
    if pending.is_empty() {
        return Ok(());
    }

    let messages = messages(pending);
    let prompt = follow_up(&messages);
    tx.log()
        .append(&session, Entry::Messages { messages })
        .await?;

    link::prompt(tx, run, prompt).await
}

/// A lone message reaches the agent as written, so a skill invocation it leads with is still
/// recognised; several are attributed, so the agent can tell who asked for what.
pub(crate) fn follow_up(messages: &[Message]) -> String {
    match messages {
        [only] => only.message.clone(),
        several => several
            .iter()
            .map(|message| format!("{}: {}", message.participant, message.message))
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

fn messages(pending: Vec<PendingMessage>) -> Vec<Message> {
    pending
        .into_iter()
        .map(|pending| Message {
            participant: pending.participant,
            message: pending.body,
        })
        .collect()
}

#[cfg(test)]
mod tests;
