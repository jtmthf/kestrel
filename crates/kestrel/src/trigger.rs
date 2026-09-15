//! An Event supplies data and never authority (ADR-0013): the Agent and the Workspace a
//! firing starts work with are named in the declaration a human applied, never in the Event.

use anyhow::{Context as _, Result, bail};

use crate::domain::{Event, EventRecordId, RunId, SessionId, Templates, Trigger, TriggerState};
use crate::fanout::{self, Change};
use crate::filter::Filter;
use crate::log::Entry;
use crate::store::Store;

/// Each firing opens a Session and enqueues a Run, so a sweep takes a bounded bite rather
/// than everything a Trigger declared over a busy repository matches at once.
const AT_A_TIME: usize = 32;

pub struct Declaration<'a> {
    pub organization: &'a str,
    pub name: &'a str,
    pub filter: &'a Filter,
    pub templates: &'a Templates,
    pub workspace: &'a str,
    pub agent: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct Fired {
    pub event: EventRecordId,
    pub session: SessionId,
    pub run: RunId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub brief: String,
    pub branch: String,
    pub correlation: Option<String>,
}

#[derive(Debug)]
pub struct Tested {
    pub matches: bool,
    pub rendered: Result<Rendered>,
}

pub async fn declare(store: &Store, declaration: Declaration<'_>) -> Result<Trigger> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(declaration.organization).await?;
    let workspace = tx
        .workspaces()
        .named(&organization, declaration.workspace)
        .await?;
    let agent = tx.agents().named(&organization, declaration.agent).await?;
    let trigger = tx
        .triggers()
        .declare(
            &organization,
            declaration.name,
            declaration.filter,
            declaration.templates,
            &workspace,
            &agent,
        )
        .await?;
    tx.commit().await?;

    Ok(trigger)
}

pub async fn triggers(store: &Store, organization: &str) -> Result<Vec<Trigger>> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.triggers().all(&organization).await
}

pub async fn show(store: &Store, organization: &str, name: &str) -> Result<Trigger> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.triggers().named(&organization, name).await
}

/// Starts nothing, so an Event recorded before the Trigger was declared, or one it already
/// fired for, is still worth asking about; it renders even when the filter does not match, so
/// a brief can be written against an Event before the filter is right.
pub async fn test(
    store: &Store,
    organization: &str,
    name: &str,
    event: EventRecordId,
) -> Result<Tested> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;
    let trigger = tx.triggers().named(&organization, name).await?;
    let event = tx.integrations().event(event).await?;
    if event.organization != organization.id {
        bail!(
            "no event {} in the organization {}",
            event.record_id,
            organization.name
        );
    }

    Ok(Tested {
        matches: tx.triggers().matches(&trigger, &event).await?,
        rendered: render(&trigger, &event),
    })
}

pub fn render(trigger: &Trigger, event: &Event) -> Result<Rendered> {
    let unrenderable = |field: &str| {
        format!(
            "the trigger {} cannot render its {field} for the event {}",
            trigger.name, event.record_id
        )
    };
    let templates = &trigger.templates;
    let occurrence = &event.occurrence;

    Ok(Rendered {
        brief: templates
            .brief
            .render(occurrence)
            .with_context(|| unrenderable("brief"))?,
        branch: match &templates.branch {
            Some(branch) => branch
                .render_line(occurrence)
                .with_context(|| unrenderable("branch"))?,
            None => trigger.workspace.branch.clone(),
        },
        correlation: templates
            .correlation
            .as_ref()
            .map(|correlation| {
                correlation
                    .render_line(occurrence)
                    .with_context(|| unrenderable("correlation"))
            })
            .transpose()?,
    })
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
        tx.triggers().unfired_matches(AT_A_TIME).await?
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
        .sessions()
        .open(
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

    let run = tx.sessions().enqueue_run(&session).await?;
    tx.triggers()
        .record_firing(trigger, event, &session)
        .await?;
    tx.commit().await?;
    fanout::publish(Change::SessionOpened(&session));

    Ok(Fired {
        event: event.record_id,
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
    let organization = tx.organizations().named(organization).await?;
    let trigger = tx.triggers().named(&organization, name).await?;
    let changed = tx.triggers().set_state(&trigger, state).await?;
    tx.commit().await?;

    Ok(changed)
}
