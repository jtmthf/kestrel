use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use jiff::Timestamp;

use crate::declined::Declined;
use crate::domain::SubscriptionProfile;
use crate::provider;
use crate::store::{Declared, Store, Tx};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Variable,
    File,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::Variable => "variable",
            Kind::File => "file",
        }
    }
}

impl FromStr for Kind {
    type Err = anyhow::Error;

    fn from_str(kind: &str) -> Result<Self> {
        match kind {
            "variable" => Ok(Kind::Variable),
            "file" => Ok(Kind::File),
            other => bail!("{other} is not something a subscription profile holds"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
}

impl Entry {
    pub fn variable(name: &str) -> Result<Self> {
        provider::named(name)?;

        Ok(Self {
            kind: Kind::Variable,
            name: name.to_owned(),
        })
    }

    pub fn file(path: &str) -> Result<Self> {
        within_a_home(path)?;

        Ok(Self {
            kind: Kind::File,
            name: path.to_owned(),
        })
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.kind.as_str(), self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub entry: Entry,
    pub set_at: Timestamp,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contents {
    pub variables: BTreeMap<String, String>,
    pub files: BTreeMap<String, String>,
}

pub async fn declare(
    store: &Store,
    organization: &str,
    name: &str,
    owner: &str,
) -> Result<Declared<SubscriptionProfile>> {
    if owner.trim().is_empty() {
        bail!(Declined::Unacceptable(
            "a subscription profile belongs to a person, and none was named".to_owned()
        ));
    }

    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;
    let declared = tx.profiles().declare(&organization, name, owner).await?;
    tx.commit().await?;

    Ok(declared)
}

pub async fn hold(
    store: &Store,
    organization: &str,
    profile: &str,
    entry: &Entry,
    secret: &str,
) -> Result<Held> {
    if secret.is_empty() {
        bail!(Declined::Unacceptable(format!(
            "a {} with nothing in it is no login",
            entry.kind.as_str()
        )));
    }

    let mut tx = store.begin().await?;
    let profile = named(&mut tx, organization, profile).await?;
    let held = tx.profiles().hold(&profile, entry, secret).await?;
    tx.commit().await?;

    Ok(held)
}

pub async fn profiles(
    store: &Store,
    organization: &str,
) -> Result<Vec<(SubscriptionProfile, Vec<Held>)>> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    let mut listed = Vec::new();
    for profile in tx.profiles().all(&organization).await? {
        let held = tx.profiles().held(&profile).await?;
        listed.push((profile, held));
    }

    Ok(listed)
}

pub async fn forget(store: &Store, organization: &str, profile: &str, entry: &Entry) -> Result<()> {
    let mut tx = store.begin().await?;
    let profile = named(&mut tx, organization, profile).await?;

    if !tx.profiles().forget(&profile, entry).await? {
        bail!(Declined::Missing(format!(
            "the subscription profile {} holds no {entry}",
            profile.name
        )));
    }

    tx.commit().await
}

/// Asked before an Instance is provisioned, so nothing is decrypted to answer it.
pub async fn holds_anything(store: &Store, profile: &SubscriptionProfile) -> Result<bool> {
    Ok(!store
        .begin()
        .await?
        .profiles()
        .held(profile)
        .await?
        .is_empty())
}

pub async fn contents(store: &Store, profile: &SubscriptionProfile) -> Result<Contents> {
    store.begin().await?.profiles().contents(profile).await
}

pub async fn refresh(
    store: &Store,
    profile: &SubscriptionProfile,
    files: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    let mut tx = store.begin().await?;
    let refreshed = tx.profiles().refresh_files(profile, files).await?;
    tx.commit().await?;

    Ok(refreshed)
}

pub(crate) async fn named(
    tx: &mut Tx<'_>,
    organization: &str,
    profile: &str,
) -> Result<SubscriptionProfile> {
    let organization = tx.organizations().named(organization).await?;

    tx.profiles().named(&organization, profile).await
}

/// A path the supervisor joins onto the agent's home, so one that could climb out of it is
/// refused where it is set rather than where it is written.
fn within_a_home(path: &str) -> Result<()> {
    let climbs = path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..");

    if path.is_empty() || path.starts_with('/') || climbs || path.contains(['\\', '\0']) {
        bail!(Declined::Unacceptable(format!(
            "{path} is not a path beneath the agent's home"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_named_by_where_it_sits_beneath_the_agents_home() {
        for accepted in [".codex/auth.json", ".local/share/opencode/auth.json", "a"] {
            assert!(Entry::file(accepted).is_ok(), "{accepted} was refused");
        }
    }

    #[test]
    fn a_path_that_could_leave_the_agents_home_is_refused() {
        for refused in [
            "",
            "/etc/passwd",
            "../outside",
            ".codex/../../outside",
            ".codex//auth.json",
            "./auth.json",
            ".codex/",
            "a\\b",
        ] {
            assert!(Entry::file(refused).is_err(), "{refused:?} was accepted");
        }
    }

    #[test]
    fn a_variable_is_one_a_process_could_carry() {
        assert!(Entry::variable("CLAUDE_CODE_OAUTH_TOKEN").is_ok());
        assert!(Entry::variable("A-KEY").is_err());
    }
}
