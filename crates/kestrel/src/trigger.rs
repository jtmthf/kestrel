//! An Event supplies data and never authority (ADR-0013): the Agent and the Workspace a
//! firing starts work with are named in the declaration a human applied, never in the Event.

use anyhow::{Context as _, Result, bail};

use crate::domain::{
    DisableReason, Event, EventRecordId, RunId, SessionId, Templates, Trigger, TriggerState,
};
use crate::fanout::{self, Change};
use crate::filter::Filter;
use crate::log::Entry;
use crate::store::session::Opening;
use crate::store::{Store, Tx};

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

#[derive(Debug, Clone)]
pub enum Fired {
    Opened {
        event: EventRecordId,
        session: SessionId,
        run: RunId,
    },
    Failed {
        event: EventRecordId,
        trigger: String,
        because: String,
    },
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
    set(
        store,
        organization,
        name,
        TriggerState::Disabled(DisableReason::Operator),
    )
    .await
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
/// that fired has work to show for it and one interrupted before committing is found again by
/// the next sweep. A firing that cannot open a Session is recorded as failed instead, because
/// the next sweep would only fail it again.
async fn firing(store: &Store, trigger: &Trigger, event: &Event) -> Result<Fired> {
    let rendered = render(trigger, event);
    let mut tx = store.begin().await?;

    if let Some(because) = tx.triggers().disabled_because(trigger).await? {
        return failed(
            tx,
            trigger,
            event,
            format!("the trigger {} is disabled: {because}", trigger.name),
        )
        .await;
    }
    if tx
        .triggers()
        .firing_budget_is_exhausted(trigger, jiff::Timestamp::now())
        .await?
    {
        let because = trigger.firing_budget_exhausted_because();
        tx.triggers()
            .set_state(trigger, TriggerState::Disabled(DisableReason::FiringBudget))
            .await?;
        return failed(tx, trigger, event, because).await;
    }

    let rendered = match rendered {
        Ok(rendered) => rendered,
        Err(error) => return failed(tx, trigger, event, format!("{error:#}")).await,
    };
    if let Some(correlation) = &rendered.correlation
        && let Some(holding) = tx
            .sessions()
            .holding_correlation(&trigger.organization, correlation)
            .await?
    {
        let because = format!(
            "the trigger {} renders the correlation {correlation} for the event {}, which the \
             open session {holding} already holds",
            trigger.name, event.record_id
        );
        return failed(tx, trigger, event, because).await;
    }

    let session = tx
        .sessions()
        .open(Opening {
            organization: &trigger.organization,
            workspace: &trigger.workspace,
            agent: &trigger.agent,
            branch: &rendered.branch,
            correlation: rendered.correlation.as_deref(),
            continues: None,
            started_by: Some(event),
        })
        .await?;

    tx.log()
        .append(
            &session,
            Entry::Brief {
                trigger: trigger.name.clone(),
                brief: rendered.brief,
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

    let run = tx.sessions().enqueue_run(&session, None).await?;
    tx.triggers()
        .record_firing(trigger, event, &session)
        .await?;
    tx.commit().await?;
    fanout::publish(Change::SessionOpened(&session));

    Ok(Fired::Opened {
        event: event.record_id,
        session: session.id,
        run: run.id,
    })
}

async fn failed(
    mut tx: Tx<'_>,
    trigger: &Trigger,
    event: &Event,
    because: String,
) -> Result<Fired> {
    tx.triggers()
        .record_failed_firing(trigger, event, &because)
        .await?;
    tx.commit().await?;

    Ok(Fired::Failed {
        event: event.record_id,
        trigger: trigger.name.clone(),
        because,
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
