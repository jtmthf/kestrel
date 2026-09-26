use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

use crate::domain::{CorrelationMiss, Fires, Templates, Trigger};
use crate::filter::Filter;
use crate::store::Store;
use crate::template::Template;
use crate::trigger::{allowed, check_miss};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub workspace: Workspace,
    pub agent: Agent,
    pub trigger: TriggerDeclaration,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub name: String,
    pub repositories: Vec<String>,
    pub branch: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub name: String,
    pub runtime: String,
    pub model: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerDeclaration {
    pub name: String,
    pub filter: serde_json::Value,
    pub brief: String,
    pub branch: Option<String>,
    pub correlation: Option<String>,
    pub on_miss: Option<String>,
    pub workspace: String,
    pub agent: String,
    #[serde(default)]
    pub allows: Vec<String>,
    pub profile: Option<String>,
}

#[derive(Serialize)]
pub struct Applied {
    pub declarations: Vec<Declaration>,
    pub admitting_outsiders: Vec<String>,
}

#[derive(Serialize)]
pub struct Declaration {
    pub kind: Kind,
    pub name: String,
    pub action: Action,
    pub differences: Vec<Difference>,
}

#[derive(Serialize)]
pub struct Difference {
    pub field: &'static str,
    pub was: Option<String>,
    pub becomes: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Workspace,
    Agent,
    Trigger,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Add,
    Change,
    Unchanged,
}

pub enum ApplyMode {
    Apply,
    Preview,
}

struct ParsedTrigger {
    filter: Filter,
    templates: Templates,
    on_miss: Option<CorrelationMiss>,
}

struct Compared {
    action: Action,
    differences: Vec<Difference>,
}

pub async fn apply(
    store: &Store,
    organization: &str,
    document: &Document,
    mode: ApplyMode,
) -> Result<Applied> {
    check_document(document)?;
    let parsed = parse_trigger(&document.trigger)?;
    let model = document
        .agent
        .model
        .as_deref()
        .filter(|model| !model.is_empty());
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    let workspaces = tx.workspaces().all(&organization).await?;
    let agents = tx.agents().all(&organization).await?;
    let triggers = tx.triggers().all(&organization).await?;
    let workspace_change = workspace_change(
        workspaces
            .iter()
            .find(|workspace| workspace.name == document.workspace.name),
        &document.workspace,
    );
    let agent_change = agent_change(
        agents
            .iter()
            .find(|agent| agent.name == document.agent.name),
        &document.agent,
        model,
    );
    let trigger_change = trigger_change(
        triggers
            .iter()
            .find(|trigger| trigger.name == document.trigger.name),
        &document.trigger,
        &parsed,
    );
    let declarations = vec![
        Declaration {
            kind: Kind::Workspace,
            name: document.workspace.name.clone(),
            action: workspace_change.action,
            differences: workspace_change.differences,
        },
        Declaration {
            kind: Kind::Agent,
            name: document.agent.name.clone(),
            action: agent_change.action,
            differences: agent_change.differences,
        },
        Declaration {
            kind: Kind::Trigger,
            name: document.trigger.name.clone(),
            action: trigger_change.action,
            differences: trigger_change.differences,
        },
    ];

    let workspace = tx
        .workspaces()
        .declare(
            &organization,
            &document.workspace.name,
            &document.workspace.repositories,
            &document.workspace.branch,
        )
        .await?
        .record;
    let agent = tx
        .agents()
        .declare(
            &organization,
            &document.agent.name,
            &document.agent.runtime,
            model,
        )
        .await?
        .record;
    let allows = allowed(&mut tx, &organization, &document.trigger.allows).await?;
    let profile = match &document.trigger.profile {
        Some(profile) => Some(tx.profiles().named(&organization, profile).await?),
        None => None,
    };
    let fires = Fires::On(parsed.filter.clone());
    match declarations[2].action {
        Action::Add => {
            tx.triggers()
                .declare(
                    &organization,
                    &document.trigger.name,
                    &fires,
                    &parsed.templates,
                    parsed.on_miss,
                    &workspace,
                    &agent,
                    &allows,
                    profile.as_ref(),
                    true,
                )
                .await?;
        }
        Action::Change => {
            let trigger = triggers
                .iter()
                .find(|trigger| trigger.name == document.trigger.name)
                .expect("a changed trigger was listed");
            tx.triggers()
                .redeclare(
                    trigger,
                    &fires,
                    &parsed.templates,
                    parsed.on_miss,
                    &workspace,
                    &agent,
                    &allows,
                    profile.as_ref(),
                    true,
                )
                .await?;
        }
        Action::Unchanged => {}
    }

    if matches!(mode, ApplyMode::Apply) {
        tx.commit().await?;
    }

    Ok(Applied {
        declarations,
        admitting_outsiders: parsed
            .filter
            .admits_outsiders()
            .then(|| document.trigger.name.clone())
            .into_iter()
            .collect(),
    })
}

fn check_document(document: &Document) -> Result<()> {
    for name in [
        &document.workspace.name,
        &document.agent.name,
        &document.trigger.name,
    ] {
        if name.is_empty() {
            bail!("a declaration names each record");
        }
    }
    if document.workspace.repositories.is_empty() {
        bail!("a workspace names at least one repository");
    }
    if let Some(clash) = sharing_a_directory(&document.workspace.repositories) {
        bail!("{clash}");
    }
    if document.workspace.branch.is_empty() {
        bail!("a workspace names the branch its work happens on");
    }
    if document.agent.runtime.is_empty() {
        bail!("an agent names the agent runtime that drives it");
    }
    if document.trigger.workspace != document.workspace.name {
        bail!(
            "the trigger names workspace {}, not the declared workspace {}",
            document.trigger.workspace,
            document.workspace.name
        );
    }
    if document.trigger.agent != document.agent.name {
        bail!(
            "the trigger names agent {}, not the declared agent {}",
            document.trigger.agent,
            document.agent.name
        );
    }
    Ok(())
}

pub(crate) fn sharing_a_directory(repositories: &[String]) -> Option<String> {
    let mut claimed = std::collections::HashMap::new();
    repositories.iter().find_map(|repository| {
        let name = repository
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(repository);
        let directory = name.strip_suffix(".git").unwrap_or(name);
        claimed.insert(directory, repository).map(|earlier| {
            format!("{earlier} and {repository} would both be checked out into {directory}")
        })
    })
}

fn parse_trigger(declaration: &TriggerDeclaration) -> Result<ParsedTrigger> {
    let templates = Templates {
        brief: declaration
            .brief
            .parse::<Template>()
            .context("a trigger brief")?,
        branch: declaration
            .branch
            .as_deref()
            .map(str::parse)
            .transpose()
            .context("a trigger branch")?,
        correlation: declaration
            .correlation
            .as_deref()
            .map(str::parse)
            .transpose()
            .context("a trigger correlation")?,
    };
    let on_miss = declaration
        .on_miss
        .as_deref()
        .map(str::parse)
        .transpose()
        .context("a trigger on_miss")?;
    check_miss(&templates, on_miss)?;

    Ok(ParsedTrigger {
        filter: Filter::from_json(&declaration.filter).context("a trigger filter")?,
        templates,
        on_miss,
    })
}

fn workspace_change(
    workspace: Option<&crate::domain::Workspace>,
    declaration: &Workspace,
) -> Compared {
    let becomes = vec![
        ("repositories", Some(declaration.repositories.join("\n"))),
        ("branch", Some(declaration.branch.clone())),
    ];
    compared(
        workspace.map(|workspace| {
            vec![
                ("repositories", Some(workspace.repositories.join("\n"))),
                ("branch", Some(workspace.branch.clone())),
            ]
        }),
        becomes,
    )
}

fn agent_change(
    agent: Option<&crate::domain::Agent>,
    declaration: &Agent,
    model: Option<&str>,
) -> Compared {
    compared(
        agent.map(|agent| {
            vec![
                ("runtime", Some(agent.runtime.clone())),
                ("model", agent.model.clone()),
            ]
        }),
        vec![
            ("runtime", Some(declaration.runtime.clone())),
            ("model", model.map(str::to_owned)),
        ],
    )
}

fn trigger_change(
    trigger: Option<&Trigger>,
    declaration: &TriggerDeclaration,
    parsed: &ParsedTrigger,
) -> Compared {
    let mut allows = declaration.allows.clone();
    allows.sort();
    allows.dedup();
    compared(
        trigger.map(described_trigger),
        vec![
            ("matches", Some(parsed.filter.to_string())),
            ("workspace", Some(declaration.workspace.clone())),
            ("agent", Some(declaration.agent.clone())),
            ("allows", (!allows.is_empty()).then(|| allows.join(", "))),
            ("profile", declaration.profile.clone()),
            (
                "branch",
                parsed.templates.branch.as_ref().map(ToString::to_string),
            ),
            (
                "correlation",
                parsed
                    .templates
                    .correlation
                    .as_ref()
                    .map(ToString::to_string),
            ),
            ("on miss", parsed.on_miss.map(|miss| miss.to_string())),
            ("brief", Some(parsed.templates.brief.to_string())),
        ],
    )
}

fn described_trigger(trigger: &Trigger) -> Vec<(&'static str, Option<String>)> {
    let mut allows = trigger
        .allows
        .iter()
        .map(|agent| agent.name.clone())
        .collect::<Vec<_>>();
    allows.sort();
    allows.dedup();
    vec![
        ("matches", Some(trigger.filter().to_string())),
        ("workspace", Some(trigger.workspace.name.clone())),
        ("agent", Some(trigger.agent.name.clone())),
        ("allows", (!allows.is_empty()).then(|| allows.join(", "))),
        (
            "profile",
            trigger.profile.as_ref().map(|profile| profile.name.clone()),
        ),
        (
            "branch",
            trigger.templates.branch.as_ref().map(ToString::to_string),
        ),
        (
            "correlation",
            trigger
                .templates
                .correlation
                .as_ref()
                .map(ToString::to_string),
        ),
        ("on miss", trigger.on_miss.map(|miss| miss.to_string())),
        ("brief", Some(trigger.templates.brief.to_string())),
    ]
}

fn compared(
    was: Option<Vec<(&'static str, Option<String>)>>,
    becomes: Vec<(&'static str, Option<String>)>,
) -> Compared {
    let added = was.is_none();
    let differences: Vec<Difference> = was.map_or_else(
        || {
            becomes
                .iter()
                .map(|(field, becomes)| Difference {
                    field,
                    was: None,
                    becomes: becomes.clone(),
                })
                .collect()
        },
        |was| {
            was.into_iter()
                .zip(&becomes)
                .filter(|((_, was), (_, becomes))| was != becomes)
                .map(|((field, was), (_, becomes))| Difference {
                    field,
                    was,
                    becomes: becomes.clone(),
                })
                .collect()
        },
    );
    let action = if added {
        Action::Add
    } else if differences.is_empty() {
        Action::Unchanged
    } else {
        Action::Change
    };

    Compared {
        action,
        differences,
    }
}
