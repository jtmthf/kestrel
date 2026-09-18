//! An Event supplies data and never authority (ADR-0013): the Agent and the Workspace a
//! firing starts work with are named in the declaration a human applied, never in the Event.

use std::collections::BTreeSet;

use anyhow::{Context as _, Result, anyhow, bail};
use jiff::Timestamp;

pub mod apply;

use crate::domain::{
    Agent, CorrelationMiss, DisableReason, Event, EventRecordId, Fires, Firing, FiringBudget,
    Occurrence, Organization, RunId, SessionId, Templates, Trigger, TriggerId, TriggerState,
};
use crate::fanout::{self, Change};
use crate::integration::github::EventData;
use crate::log::Entry;
use crate::session;
use crate::store::integration::Recorded;
use crate::store::session::Opening;
use crate::store::{Store, Tx};

/// A sweep takes a bounded bite rather than every Event a Trigger declared over a busy repository
/// matches at once.
const AT_A_TIME: usize = 32;

pub const AGENT_LABEL: &str = "agent:";

pub struct Declaration<'a> {
    pub organization: &'a str,
    pub name: &'a str,
    pub fires: &'a Fires,
    pub templates: &'a Templates,
    pub on_miss: Option<CorrelationMiss>,
    pub workspace: &'a str,
    pub agent: &'a str,
    pub allows: &'a [String],
}

#[derive(Debug, Clone)]
pub enum Fired {
    Opened {
        event: EventRecordId,
        session: SessionId,
        run: RunId,
    },
    Fed {
        event: EventRecordId,
        session: SessionId,
        run: Option<RunId>,
    },
    Ignored {
        event: EventRecordId,
        trigger: String,
        correlation: String,
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
    pub branch: Option<String>,
    pub correlation: Option<String>,
}

#[derive(Debug)]
pub struct Tested {
    pub matches: bool,
    pub rendered: Result<Rendered>,
    pub agent: Result<String>,
    /// When the elapsing a test named no Event for is due.
    pub elapsing: Option<Timestamp>,
}

pub(crate) fn check_miss(templates: &Templates, on_miss: Option<CorrelationMiss>) -> Result<()> {
    match (templates.correlation.is_some(), on_miss) {
        (true, None) => {
            bail!("a trigger with a correlation must declare what it does when it misses")
        }
        (false, Some(_)) => {
            bail!("a trigger without a correlation cannot declare what it does when it misses")
        }
        _ => Ok(()),
    }
}

pub async fn declare(store: &Store, declaration: Declaration<'_>) -> Result<Trigger> {
    check_miss(declaration.templates, declaration.on_miss)?;
    if let Fires::Every(every) = declaration.fires {
        let budget = FiringBudget::default();
        let fastest = budget.window / i32::try_from(budget.limit.get())?;
        if *every < fastest {
            bail!(
                "a trigger firing every {every:#} would exhaust its budget of {} firings in {:#}: \
                 fire at most every {fastest:#}",
                budget.limit,
                budget.window
            );
        }
    }

    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(declaration.organization).await?;
    let workspace = tx
        .workspaces()
        .named(&organization, declaration.workspace)
        .await?;
    let agent = tx.agents().named(&organization, declaration.agent).await?;
    let allows = allowed(&mut tx, &organization, declaration.allows).await?;
    let trigger = tx
        .triggers()
        .declare(
            &organization,
            declaration.name,
            declaration.fires,
            declaration.templates,
            declaration.on_miss,
            &workspace,
            &agent,
            &allows,
            false,
        )
        .await?;
    tx.commit().await?;

    Ok(trigger)
}

pub(crate) async fn allowed(
    tx: &mut Tx<'_>,
    organization: &Organization,
    names: &[String],
) -> Result<Vec<Agent>> {
    let mut allows = Vec::with_capacity(names.len());
    for name in names {
        allows.push(tx.agents().named(organization, name).await?);
    }

    Ok(allows)
}

/// The Trigger's own Agent, or the one an `agent:<name>` label on the work item chooses from
/// those it allows: a label is data, so it never reaches an Agent a human did not name here.
pub fn chosen<'t>(trigger: &'t Trigger, event: &Event) -> Result<&'t Agent> {
    let data = EventData::new(&event.occurrence);
    let named: BTreeSet<&str> = data
        .labels()
        .filter_map(|label| label.strip_prefix(AGENT_LABEL))
        .collect();

    match named.into_iter().collect::<Vec<_>>().as_slice() {
        [] => Ok(&trigger.agent),
        [name] => std::iter::once(&trigger.agent)
            .chain(&trigger.allows)
            .find(|agent| agent.name == *name)
            .ok_or_else(|| {
                anyhow!(
                    "the label {AGENT_LABEL}{name} chooses an agent the trigger {} does not allow",
                    trigger.name
                )
            }),
        several => bail!(
            "the labels {} each choose an agent, and the trigger {} will not guess which",
            several
                .iter()
                .map(|name| format!("{AGENT_LABEL}{name}"))
                .collect::<Vec<_>>()
                .join(" and "),
            trigger.name
        ),
    }
}

pub async fn firings(store: &Store, event: EventRecordId) -> Result<Vec<Firing>> {
    store.begin().await?.triggers().firings_of(event).await
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
    event: Option<EventRecordId>,
) -> Result<Tested> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;
    let trigger = tx.triggers().named(&organization, name).await?;

    tested(&mut tx, &trigger, event).await
}

pub async fn test_declared(
    store: &Store,
    organization: &str,
    declared: &apply::Declared,
    event: Option<EventRecordId>,
) -> Result<Tested> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;
    let trigger = Trigger {
        id: TriggerId::generate(),
        workspace: tx
            .workspaces()
            .named(&organization, &declared.workspace)
            .await?,
        agent: tx.agents().named(&organization, &declared.agent).await?,
        allows: allowed(&mut tx, &organization, &declared.allows).await?,
        organization,
        name: declared.name.clone(),
        fires: Fires::On(declared.filter.clone()),
        templates: declared.templates.clone(),
        on_miss: declared.on_miss,
        state: TriggerState::Enabled,
        disabled_because: None,
        firing_budget: FiringBudget::default(),
        applied: true,
        declared_at: jiff::Timestamp::now(),
    };

    tested(&mut tx, &trigger, event).await
}

async fn tested(
    tx: &mut Tx<'_>,
    trigger: &Trigger,
    event: Option<EventRecordId>,
) -> Result<Tested> {
    let Some(event) = event else {
        let due = tx.triggers().due_at(trigger).await?.with_context(|| {
            format!(
                "the trigger {} fires on events, so a test names one",
                trigger.name
            )
        })?;
        let event = Event {
            record_id: EventRecordId::generate(),
            organization: trigger.organization.id,
            integration: None,
            occurrence: trigger
                .elapsing(due)
                .context("a trigger with a due time has a schedule")?,
            recorded_at: Timestamp::now(),
        };
        return Ok(Tested {
            matches: true,
            rendered: render(trigger, &event),
            agent: chosen_name(trigger, &event),
            elapsing: Some(due),
        });
    };

    let event = tx.integrations().event(event).await?;
    if event.organization != trigger.organization.id {
        bail!(
            "no event {} in the organization {}",
            event.record_id,
            trigger.organization.name
        );
    }

    Ok(Tested {
        matches: tx.triggers().matches(trigger, &event).await?,
        rendered: render(trigger, &event),
        agent: chosen_name(trigger, &event),
        elapsing: None,
    })
}

fn chosen_name(trigger: &Trigger, event: &Event) -> Result<String> {
    chosen(trigger, event).map(|agent| agent.name.clone())
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
        branch: templates
            .branch
            .as_ref()
            .map(|branch| {
                branch
                    .render_line(occurrence)
                    .with_context(|| unrenderable("branch"))
            })
            .transpose()?,
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

/// Records what each due schedule mints and leaves the firing to [`fire`], so scheduled work
/// is recorded and fired like any other Event. Elapsings missed while nothing swept coalesce
/// into one.
pub async fn elapse(store: &Store, at: Timestamp) -> Result<Vec<Occurrence>> {
    let mut tx = store.begin().await?;
    let mut minted = Vec::new();

    for (trigger, due) in tx.triggers().schedules_due(at).await? {
        let Fires::Every(every) = trigger.fires else {
            bail!("the trigger {} is due but has no schedule", trigger.name);
        };
        let occurrence = trigger
            .elapsing(due)
            .context("a scheduled trigger mints an event")?;
        if let Recorded::Recorded = tx
            .integrations()
            .record_minted(&trigger.organization, &occurrence)
            .await?
        {
            minted.push(occurrence);
        }

        let missed = at.duration_since(due).as_nanos() / every.as_nanos();
        let next =
            Timestamp::from_nanosecond(due.as_nanosecond() + (missed + 1) * every.as_nanos())?;
        tx.triggers().due_again(&trigger, next).await?;
    }
    tx.commit().await?;

    Ok(minted)
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

/// An opening firing atomically commits its Session, first entry, Run and record, so a retry
/// never opens its work twice.
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
    let continues = if let Some(correlation) = &rendered.correlation {
        if let Some(holding) = tx
            .sessions()
            .holding_correlation(&trigger.organization, correlation)
            .await?
        {
            return fed(tx, trigger, event, &rendered, holding).await;
        }

        // A key a sealed session held is kestrel's own work, so `ignore` does not drop it.
        let sealed = tx
            .sessions()
            .sealed_holding_correlation(&trigger.organization, correlation)
            .await?;
        if sealed.is_none() && trigger.on_miss == Some(CorrelationMiss::Ignore) {
            return ignored(tx, trigger, event, correlation).await;
        }
        sealed
    } else {
        None
    };

    let agent = match chosen(trigger, event) {
        Ok(agent) => agent,
        Err(error) => return failed(tx, trigger, event, format!("{error:#}")).await,
    };
    let session = tx
        .sessions()
        .open(Opening {
            organization: &trigger.organization,
            workspace: &trigger.workspace,
            agent,
            branch: continues
                .as_ref()
                .map(|sealed| sealed.checkout.branch.as_str())
                .or(rendered.branch.as_deref()),
            correlation: rendered.correlation.as_deref(),
            continues: continues.as_ref(),
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
                participant: agent.name.clone(),
            },
        )
        .await?;

    let run = tx.sessions().enqueue_run(&session, None).await?;
    tx.triggers()
        .record_opened_firing(trigger, event, &session)
        .await?;
    tx.commit().await?;
    fanout::publish(Change::SessionOpened(&session));

    Ok(Fired::Opened {
        event: event.record_id,
        session: session.id,
        run: run.id,
    })
}

async fn fed(
    mut tx: Tx<'_>,
    trigger: &Trigger,
    event: &Event,
    rendered: &Rendered,
    holding: SessionId,
) -> Result<Fired> {
    let session = tx.sessions().get(holding).await?;
    let run = session::post_in(&mut tx, &session, &trigger.name, &rendered.brief).await?;
    tx.triggers()
        .record_fed_firing(trigger, event, &session)
        .await?;
    tx.commit().await?;

    Ok(Fired::Fed {
        event: event.record_id,
        session: session.id,
        run: run.map(|run| run.id),
    })
}

async fn ignored(
    mut tx: Tx<'_>,
    trigger: &Trigger,
    event: &Event,
    correlation: &str,
) -> Result<Fired> {
    tx.triggers().record_ignored_firing(trigger, event).await?;
    tx.commit().await?;

    Ok(Fired::Ignored {
        event: event.record_id,
        trigger: trigger.name.clone(),
        correlation: correlation.to_owned(),
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
