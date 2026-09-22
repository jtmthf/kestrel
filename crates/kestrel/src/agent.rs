//! An Agent's model is configuration rather than a rebuild: declared, changed, and refused
//! against what its Agent Runtime has been seen to advertise (ADR-0007).

use std::fmt;

use anyhow::Result;

use crate::domain::{Agent, Organization};
use crate::store::{Declared, Store, Tx};

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
    advertised(&mut tx, &organization, runtime, model).await?;

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
    advertised(&mut tx, &organization, &agent.runtime, model).await?;

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

/// A model a Run would fail on is refused here instead, where saying so costs nothing. What a
/// runtime advertises is only ever learned from a Run, so one no Run has reached yet is taken
/// at its word.
pub(crate) async fn advertised(
    tx: &mut Tx<'_>,
    organization: &Organization,
    runtime: &str,
    model: Option<&str>,
) -> Result<()> {
    let Some(model) = model else {
        return Ok(());
    };
    let advertised = tx
        .agents()
        .models_advertised(organization.id, runtime)
        .await?;

    if advertised.is_empty() || advertised.iter().any(|offered| offered == model) {
        return Ok(());
    }

    Err(NotOffered {
        runtime: runtime.to_owned(),
        model: model.to_owned(),
        advertised,
    }
    .into())
}

#[derive(Debug)]
pub struct NotOffered {
    pub runtime: String,
    pub model: String,
    pub advertised: Vec<String>,
}

impl fmt::Display for NotOffered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the agent runtime {} offers {}, and not {}",
            self.runtime,
            self.advertised.join(", "),
            self.model
        )
    }
}

impl std::error::Error for NotOffered {}
