//! Scope is derived for every invocation and never stored (ADR-0016).

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::api::ControlPlane;
use crate::{BINARY, ORGANIZATION_VARIABLE, names};

const BINDING: &str = ".kestrel/organization";

pub struct Scope {
    pub organization: String,
    pub source: Source,
}

pub enum Source {
    Flag,
    Environment,
    Binding(PathBuf),
    OnlyOrganization,
}

impl fmt::Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Flag => formatter.write_str("--organization"),
            Source::Environment => formatter.write_str(ORGANIZATION_VARIABLE),
            Source::Binding(binding) => write!(formatter, "{}", binding.display()),
            Source::OnlyOrganization => formatter.write_str("only organization"),
        }
    }
}

pub enum Derived {
    Scope(Scope),
    Unnamed { existing: Vec<String> },
}

pub struct Scoping<'a> {
    api: &'a ControlPlane,
    named: Option<Scope>,
}

impl<'a> Scoping<'a> {
    pub fn new(api: &'a ControlPlane, named: Option<Scope>) -> Self {
        Self { api, named }
    }

    pub async fn resolve(self) -> Result<Scope> {
        match self.derive().await? {
            Derived::Scope(scope) => Ok(scope),
            Derived::Unnamed { existing } if existing.is_empty() => bail!(
                "no Organization is in scope and none exists; declare one with \
                 `{BINARY} organization declare <name>`"
            ),
            Derived::Unnamed { existing } => bail!(
                "no Organization is in scope and {} exist: {}; pass --organization <name>",
                existing.len(),
                existing.join(", ")
            ),
        }
    }

    pub async fn derive(self) -> Result<Derived> {
        if let Some(scope) = self.named {
            return Ok(Derived::Scope(scope));
        }
        if let Some(scope) = bound()? {
            return Ok(Derived::Scope(scope));
        }

        let mut existing = names(&self.api.get(&["organizations"]).await?);
        if existing.len() == 1 {
            return Ok(Derived::Scope(Scope {
                organization: existing.remove(0),
                source: Source::OnlyOrganization,
            }));
        }

        Ok(Derived::Unnamed { existing })
    }
}

fn bound() -> Result<Option<Scope>> {
    let working = std::env::current_dir().context("reading the working directory")?;
    let Some(binding) = binding_for(&working) else {
        return Ok(None);
    };

    let organization = std::fs::read_to_string(&binding)
        .with_context(|| format!("reading {}", binding.display()))?
        .trim()
        .to_owned();
    if organization.is_empty() {
        bail!("{} binds no Organization", binding.display());
    }

    Ok(Some(Scope {
        organization,
        source: Source::Binding(binding),
    }))
}

// The search stops at the repository root, so a binding in a home directory never becomes a
// remembered current Organization.
fn binding_for(working: &Path) -> Option<PathBuf> {
    let root = working
        .ancestors()
        .find(|directory| directory.join(".git").exists());

    for directory in working.ancestors() {
        let binding = directory.join(BINDING);
        if binding.is_file() {
            return Some(binding);
        }
        if root.is_none_or(|root| directory == root) {
            break;
        }
    }

    None
}
