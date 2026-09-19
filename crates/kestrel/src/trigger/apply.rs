use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use serde::Deserialize;

use crate::domain::{CorrelationMiss, Fires, Templates, Trigger};
use crate::filter::Filter;
use crate::store::Store;
use crate::trigger::{allowed, check_miss};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub name: String,
    pub filter: Filter,
    pub templates: Templates,
    pub on_miss: Option<CorrelationMiss>,
    pub workspace: String,
    pub agent: String,
    pub allows: Vec<String>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    pub action: Action,
    pub differences: Vec<Difference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Add,
    Change,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    pub field: &'static str,
    pub was: Option<String>,
    pub becomes: Option<String>,
}

#[derive(Debug)]
pub struct Applied {
    pub changes: Vec<Change>,
    pub admitting_outsiders: Vec<String>,
}

/// `triggers` is required, so an empty file is a mistake rather than every applied trigger
/// removed; `triggers: {}` says that on purpose.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    triggers: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    filter: serde_json::Value,
    brief: String,
    branch: Option<String>,
    correlation: Option<String>,
    on_miss: Option<String>,
    workspace: String,
    agent: String,
    #[serde(default)]
    allows: Vec<String>,
    profile: Option<String>,
}

pub fn parse(text: &str) -> Result<Vec<Declared>> {
    let file: File = yaml_serde::from_str(text).context("reading the declaration file")?;

    file.triggers
        .into_iter()
        .map(|(name, entry)| {
            let declared = declared(&name, entry);
            declared.with_context(|| format!("the trigger {name} in the declaration file"))
        })
        .collect()
}

fn declared(name: &str, entry: Entry) -> Result<Declared> {
    let templates = Templates {
        brief: entry.brief.parse().context("its brief")?,
        branch: entry
            .branch
            .map(|branch| branch.parse().context("its branch"))
            .transpose()?,
        correlation: entry
            .correlation
            .map(|correlation| correlation.parse().context("its correlation"))
            .transpose()?,
    };
    let on_miss = entry.on_miss.map(|miss| miss.parse()).transpose()?;
    check_miss(&templates, on_miss)?;

    Ok(Declared {
        name: name.to_owned(),
        filter: Filter::from_json(&entry.filter).context("its filter")?,
        templates,
        on_miss,
        workspace: entry.workspace,
        agent: entry.agent,
        allows: entry.allows,
        profile: entry.profile,
    })
}

/// One transaction, so a declaration naming a workspace or agent that does not exist changes
/// nothing; a dry run is that transaction rolled back.
pub async fn apply(
    store: &Store,
    organization: &str,
    declarations: &[Declared],
    dry_run: bool,
) -> Result<Applied> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;
    let existing = tx.triggers().all(&organization).await?;
    let mut changes = Vec::new();

    for declared in declarations {
        let workspace = tx
            .workspaces()
            .named(&organization, &declared.workspace)
            .await?;
        let agent = tx.agents().named(&organization, &declared.agent).await?;
        let allows = allowed(&mut tx, &organization, &declared.allows).await?;
        let profile = match &declared.profile {
            Some(profile) => Some(tx.profiles().named(&organization, profile).await?),
            None => None,
        };
        let becomes = described(declared);
        let fires = Fires::On(declared.filter.clone());

        let Some(trigger) = existing
            .iter()
            .find(|trigger| trigger.name == declared.name)
        else {
            tx.triggers()
                .declare(
                    &organization,
                    &declared.name,
                    &fires,
                    &declared.templates,
                    declared.on_miss,
                    &workspace,
                    &agent,
                    &allows,
                    profile.as_ref(),
                    true,
                )
                .await?;
            changes.push(Change {
                name: declared.name.clone(),
                action: Action::Add,
                differences: differences(&absent(&becomes), &becomes),
            });
            continue;
        };

        let mut changed = differences(&described_trigger(trigger), &becomes);
        if !changed.is_empty() {
            tx.triggers()
                .redeclare(
                    trigger,
                    &fires,
                    &declared.templates,
                    declared.on_miss,
                    &workspace,
                    &agent,
                    &allows,
                    profile.as_ref(),
                    true,
                )
                .await?;
        } else if !trigger.applied {
            tx.triggers().adopt(trigger).await?;
        }
        if !trigger.applied {
            changed.insert(
                0,
                Difference {
                    field: "declared by",
                    was: Some("flags".to_owned()),
                    becomes: Some("a file".to_owned()),
                },
            );
        }
        if !changed.is_empty() {
            changes.push(Change {
                name: declared.name.clone(),
                action: Action::Change,
                differences: changed,
            });
        }
    }

    for trigger in &existing {
        if trigger.applied
            && !declarations
                .iter()
                .any(|declared| declared.name == trigger.name)
        {
            tx.triggers().remove(trigger).await?;
            changes.push(Change {
                name: trigger.name.clone(),
                action: Action::Remove,
                differences: Vec::new(),
            });
        }
    }

    if !dry_run {
        tx.commit().await?;
    }
    changes.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Applied {
        changes,
        admitting_outsiders: declarations
            .iter()
            .filter(|declared| declared.filter.admits_outsiders())
            .map(|declared| declared.name.clone())
            .collect(),
    })
}

type Described = [(&'static str, Option<String>); 9];

fn described(declared: &Declared) -> Described {
    describe(
        &declared.filter,
        &declared.templates,
        declared.on_miss,
        &declared.workspace,
        &declared.agent,
        &declared.allows,
        declared.profile.as_deref(),
    )
}

fn described_trigger(trigger: &Trigger) -> Described {
    describe(
        &trigger.filter(),
        &trigger.templates,
        trigger.on_miss,
        &trigger.workspace.name,
        &trigger.agent.name,
        &trigger
            .allows
            .iter()
            .map(|agent| agent.name.clone())
            .collect::<Vec<_>>(),
        trigger
            .profile
            .as_ref()
            .map(|profile| profile.name.as_str()),
    )
}

fn describe(
    filter: &Filter,
    templates: &Templates,
    on_miss: Option<CorrelationMiss>,
    workspace: &str,
    agent: &str,
    allows: &[String],
    profile: Option<&str>,
) -> Described {
    let mut allows = allows.to_vec();
    allows.sort();
    allows.dedup();

    [
        ("matches", Some(filter.to_string())),
        ("workspace", Some(workspace.to_owned())),
        ("agent", Some(agent.to_owned())),
        ("allows", (!allows.is_empty()).then(|| allows.join(", "))),
        ("profile", profile.map(str::to_owned)),
        ("branch", templates.branch.as_ref().map(ToString::to_string)),
        (
            "correlation",
            templates.correlation.as_ref().map(ToString::to_string),
        ),
        ("on miss", on_miss.map(|miss| miss.to_string())),
        ("brief", Some(templates.brief.to_string())),
    ]
}

fn absent(described: &Described) -> Described {
    described.clone().map(|(field, _)| (field, None))
}

fn differences(was: &Described, becomes: &Described) -> Vec<Difference> {
    was.iter()
        .zip(becomes)
        .filter(|((_, was), (_, becomes))| was != becomes)
        .map(|((field, was), (_, becomes))| Difference {
            field,
            was: was.clone(),
            becomes: becomes.clone(),
        })
        .collect()
}
