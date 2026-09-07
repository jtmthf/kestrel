//! A Provider Credential is held by the Organization rather than by an Agent, and is encrypted
//! at rest with the key beside the database.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use jiff::Timestamp;

use crate::domain::OrganizationId;
use crate::store::Store;

/// A credential as everything but the spawn that carries it sees it: what it is read from,
/// and never what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub variable: String,
    pub set_at: Timestamp,
}

pub async fn hold(store: &Store, organization: &str, variable: &str, secret: &str) -> Result<()> {
    named(variable)?;
    if secret.is_empty() {
        bail!("a provider credential with nothing in it is not one");
    }

    let mut tx = store.begin().await?;
    let organization = tx.organization_named(organization).await?;
    tx.hold_provider_credential(organization.id, variable, secret)
        .await?;

    tx.commit().await
}

pub async fn held(store: &Store, organization: &str) -> Result<Vec<Held>> {
    let mut tx = store.begin().await?;
    let organization = tx.organization_named(organization).await?;

    tx.provider_credentials_held(organization.id).await
}

pub async fn forget(store: &Store, organization: &str, variable: &str) -> Result<()> {
    let mut tx = store.begin().await?;
    let organization = tx.organization_named(organization).await?;

    if !tx
        .forget_provider_credential(organization.id, variable)
        .await?
    {
        bail!(
            "the organization {} holds no provider credential named {variable}",
            organization.name
        );
    }

    tx.commit().await
}

/// Asked before an Environment is provisioned, so nothing is decrypted to answer it.
pub async fn holds_any(store: &Store, organization: OrganizationId) -> Result<bool> {
    Ok(!store
        .begin()
        .await?
        .provider_credentials_held(organization)
        .await?
        .is_empty())
}

pub async fn reaching(
    store: &Store,
    organization: OrganizationId,
) -> Result<BTreeMap<String, String>> {
    store
        .begin()
        .await?
        .provider_credentials(organization)
        .await
}

/// A credential is named by the environment variable the runtime reads it from, so a name a
/// process could not carry is refused where it is set rather than where it is spawned.
fn named(variable: &str) -> Result<()> {
    let acceptable = variable
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_');
    let starts = variable
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_');

    if !acceptable || !starts {
        bail!("{variable} is not an environment variable an Agent Runtime could be spawned with");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credential_is_named_by_the_variable_a_runtime_reads_it_from() {
        assert!(named("ANTHROPIC_API_KEY").is_ok());
        assert!(named("_KEY2").is_ok());
    }

    #[test]
    fn a_name_no_process_could_carry_is_refused() {
        for refused in ["", "2KEY", "A KEY", "A=KEY", "A-KEY", "clé"] {
            assert!(named(refused).is_err(), "{refused} was accepted");
        }
    }
}
