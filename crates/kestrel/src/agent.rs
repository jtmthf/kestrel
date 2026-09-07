//! An Agent's model is configuration rather than a rebuild: declared, changed, and refused
//! against what its Agent Runtime has been seen to advertise (ADR-0007).

use anyhow::{Result, bail};

use crate::domain::{Agent, Organization};
use crate::store::{Store, Tx};

pub async fn declare(
    store: &Store,
    organization: &str,
    name: &str,
    runtime: &str,
    model: Option<&str>,
) -> Result<Agent> {
    let model = names(model);
    let mut tx = store.begin().await?;
    let organization = tx.organization_named(organization).await?;
    advertised(&mut tx, &organization, runtime, model).await?;

    let agent = tx
        .declare_agent(&organization, name, runtime, model)
        .await?;
    tx.commit().await?;

    Ok(agent)
}

pub async fn set_model(
    store: &Store,
    organization: &str,
    name: &str,
    model: Option<&str>,
) -> Result<Agent> {
    let model = names(model);
    let mut tx = store.begin().await?;
    let organization = tx.organization_named(organization).await?;
    let agent = tx.agent_named(&organization, name).await?;
    advertised(&mut tx, &organization, &agent.runtime, model).await?;

    let agent = tx.set_agent_model(&agent, model).await?;
    tx.commit().await?;

    Ok(agent)
}

pub async fn agents(store: &Store, organization: &str) -> Result<Vec<Agent>> {
    let mut tx = store.begin().await?;
    let organization = tx.organization_named(organization).await?;

    tx.agents(&organization).await
}

/// A model named as nothing is a model nobody named.
fn names(model: Option<&str>) -> Option<&str> {
    model.filter(|model| !model.is_empty())
}

/// A model a Run would fail on is refused here instead, where saying so costs nothing. What a
/// runtime advertises is only ever learned from a Run, so one no Run has reached yet is taken
/// at its word.
async fn advertised(
    tx: &mut Tx<'_>,
    organization: &Organization,
    runtime: &str,
    model: Option<&str>,
) -> Result<()> {
    let Some(model) = model else {
        return Ok(());
    };
    let advertised = tx.models_advertised(organization.id, runtime).await?;

    if advertised.is_empty() || advertised.iter().any(|offered| offered == model) {
        return Ok(());
    }

    bail!(
        "the agent runtime {runtime} offers {}, and not {model}",
        advertised.join(", ")
    )
}
