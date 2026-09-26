//! An Agent's model is configuration rather than a rebuild: declared, changed, and taken at
//! its word, because what a runtime offers is only learned from a Run, and a Subscription
//! Profile added since can widen it (ADR-0007).

use anyhow::Result;

use crate::domain::Agent;
use crate::store::{Declared, Store};

pub async fn declare(
    store: &Store,
    organization: &str,
    name: &str,
    runtime: &str,
    model: Option<&str>,
) -> Result<Declared<Agent>> {
    let model = names(model);
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    let declared = tx
        .agents()
        .declare(&organization, name, runtime, model)
        .await?;
    tx.commit().await?;

    Ok(declared)
}

pub async fn set_model(
    store: &Store,
    organization: &str,
    name: &str,
    model: Option<&str>,
) -> Result<Agent> {
    let model = names(model);
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;
    let agent = tx.agents().named(&organization, name).await?;

    let agent = tx.agents().set_model(&agent, model).await?;
    tx.commit().await?;

    Ok(agent)
}

pub async fn agents(store: &Store, organization: &str) -> Result<Vec<Agent>> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.agents().all(&organization).await
}

/// A model named as nothing is a model nobody named.
fn names(model: Option<&str>) -> Option<&str> {
    model.filter(|model| !model.is_empty())
}
