//! An Event supplies data and never authority (ADR-0013): the Agent and the Workspace a
//! firing starts work with are named in the declaration a human applied, never in the Event.

use anyhow::{Result, bail};

use crate::domain::{Event, EventId, RunId, SessionId, Trigger, TriggerState};
use crate::fanout::{self, Change};
use crate::integration::github;
use crate::log::Entry;
use crate::store::Store;

/// Each firing opens a Session and enqueues a Run, so a sweep takes a bounded bite rather
/// than everything a Trigger declared over a busy repository matches at once.
const AT_A_TIME: usize = 32;

pub struct Declaration<'a> {
    pub organization: &'a str,
    pub name: &'a str,
    pub repository: &'a str,
    pub label: &'a str,
    pub workspace: &'a str,
    pub agent: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct Fired {
    pub event: EventId,
    pub session: SessionId,
    pub run: RunId,
}

pub async fn declare(store: &Store, declaration: Declaration<'_>) -> Result<Trigger> {
    if declaration.label.is_empty() {
        bail!("a trigger matches a label: name the one it fires on");
    }
    let repository = github::repository(declaration.repository)?;

    let mut tx = store.begin().await?;
    let organization = tx.organization_named(declaration.organization).await?;
    let workspace = tx
        .workspace_named(&organization, declaration.workspace)
        .await?;
    let agent = tx.agent_named(&organization, declaration.agent).await?;
    let trigger = tx
        .declare_trigger(
            &organization,
            declaration.name,
            (&repository, declaration.label),
            &workspace,
            &agent,
        )
        .await?;
    tx.commit().await?;

    Ok(trigger)
}

pub async fn triggers(store: &Store, organization: &str) -> Result<Vec<Trigger>> {
    let mut tx = store.begin().await?;
    let organization = tx.organization_named(organization).await?;

    tx.triggers(&organization).await
}

pub async fn show(store: &Store, organization: &str, name: &str) -> Result<Trigger> {
    let mut tx = store.begin().await?;
    let organization = tx.organization_named(organization).await?;

    tx.trigger_named(&organization, name).await
}

pub async fn disable(store: &Store, organization: &str, name: &str) -> Result<Trigger> {
    set(store, organization, name, TriggerState::Disabled).await
}

pub async fn enable(store: &Store, organization: &str, name: &str) -> Result<Trigger> {
    set(store, organization, name, TriggerState::Enabled).await
}

/// An Event no Trigger matches opens nothing, and that is not a failure.
pub async fn fire(store: &Store) -> Result<Vec<Fired>> {
    let matched = {
        let mut tx = store.begin().await?;
        tx.unfired_matches(github::LABELLED, AT_A_TIME).await?
    };

    let mut fired = Vec::with_capacity(matched.len());
    for (trigger, event) in matched {
        fired.push(firing(store, &trigger, &event).await?);
    }

    Ok(fired)
}

/// The Session, its first entry, the Run and the firing itself commit together, so a Trigger
/// that fired has work to show for it and one that did not is found again by the next sweep.
async fn firing(store: &Store, trigger: &Trigger, event: &Event) -> Result<Fired> {
    let mut tx = store.begin().await?;
    let session = tx
        .open_session(
            &trigger.organization,
            &trigger.workspace,
            &trigger.agent,
            None,
            Some(event),
        )
        .await?;

    tx.log()
        .append(
            &session,
            Entry::TriggerFired {
                trigger: trigger.name.clone(),
                repository: event.repository.clone(),
                occurrence: event.occurrence.clone(),
            },
        )
        .await?;
    tx.log()
        .append(
            &session,
            Entry::ParticipantJoined {
                participant: trigger.agent.name.clone(),
            },
        )
        .await?;

    let run = tx.enqueue_run(&session).await?;
    tx.record_firing(trigger, event, &session).await?;
    tx.commit().await?;
    fanout::publish(Change::SessionOpened(&session));

    Ok(Fired {
        event: event.id,
        session: session.id,
        run: run.id,
    })
}

async fn set(
    store: &Store,
    organization: &str,
    name: &str,
    state: TriggerState,
) -> Result<Trigger> {
    let mut tx = store.begin().await?;
    let organization = tx.organization_named(organization).await?;
    let trigger = tx.trigger_named(&organization, name).await?;
    let changed = tx.set_trigger_state(&trigger, state).await?;
    tx.commit().await?;

    Ok(changed)
}
