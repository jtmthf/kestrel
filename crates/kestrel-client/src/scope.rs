//! Organization scope is derived fresh for every invocation and never stored (ADR-0016).

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};

use crate::api::ControlPlane;

/// The environment variable that names the Organization when no flag does.
pub const ORGANIZATION: &str = "KESTREL_ORGANIZATION";

/// A committed binding in the working directory, so the scope is reviewed in a pull request
/// rather than remembered in a hidden profile.
const BINDING: &str = ".kestrel/organization";

/// The Organization this invocation applies to, and where it came from.
pub struct Scope {
    pub organization: String,
    /// The flag, environment variable, or committed binding that named it — or the fact that
    /// it was the only Organization there was.
    pub source: String,
    /// The committed binding read, when one was.
    pub binding: Option<PathBuf>,
}

/// Resolves the invocation's Organization in the fixed order the flag, the environment, a
/// committed binding, then the only Organization. The control plane is reached only for the
/// last step, and only to ask what exists.
pub async fn resolve(api: &ControlPlane, given: Option<&str>) -> Result<Scope> {
    if let Some(given) = given {
        let organization = given.trim();
        if organization.is_empty() {
            bail!("--organization names no Organization");
        }

        return Ok(Scope {
            organization: organization.to_owned(),
            source: "--organization".to_owned(),
            binding: None,
        });
    }
    if let Some(organization) = environment() {
        return Ok(Scope {
            organization,
            source: ORGANIZATION.to_owned(),
            binding: None,
        });
    }
    if let Some((binding, organization)) = committed()? {
        return Ok(Scope {
            source: binding.display().to_string(),
            organization,
            binding: Some(binding),
        });
    }

    let listed = api.get(&["organizations"]).await?;
    let names: Vec<&str> = listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|organization| organization["name"].as_str())
        .collect();

    match names.as_slice() {
        [] => bail!(
            "no Organization is in scope and none exists; declare one with \
             `kestrel-client organization declare <name>`"
        ),
        [organization] => Ok(Scope {
            organization: (*organization).to_owned(),
            source: "only organization".to_owned(),
            binding: None,
        }),
        _ => bail!(
            "no Organization is in scope and {} exist: {}; pass --organization <name>",
            names.len(),
            names.join(", ")
        ),
    }
}

fn environment() -> Option<String> {
    std::env::var(ORGANIZATION)
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// A committed binding in the working directory itself.
fn committed() -> Result<Option<(PathBuf, String)>> {
    let binding = std::env::current_dir()
        .context("the working directory")?
        .join(BINDING);
    if !binding.is_file() {
        return Ok(None);
    }

    let organization = std::fs::read_to_string(&binding)
        .with_context(|| format!("reading {}", binding.display()))?;
    let organization = organization.trim().to_owned();
    if organization.is_empty() {
        bail!("{} binds no Organization", binding.display());
    }

    Ok(Some((binding, organization)))
}
