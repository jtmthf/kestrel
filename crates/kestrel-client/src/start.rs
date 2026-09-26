use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::scope::{Scope, Source};

const DEFAULT_ORGANIZATION: &str = "default";
const DEFAULT_HARNESS: &str = "opencode";

#[derive(Default)]
pub struct Given {
    pub repositories: Vec<String>,
    pub branch: Option<String>,
    pub project: Option<String>,
    pub agent: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub credentials: Vec<String>,
}

#[derive(Default)]
pub struct Existing {
    pub organization_declared: bool,
    pub projects: Vec<Project>,
    pub agents: Vec<Agent>,
    pub credentials: Vec<String>,
}

#[derive(Default)]
pub struct LocalClone {
    pub root: Option<PathBuf>,
    pub origin: Option<String>,
    pub default_branch: Option<String>,
    pub checked_out: Option<String>,
}

impl Existing {
    pub fn read(
        organization_declared: bool,
        projects: &Value,
        agents: &Value,
        credentials: &Value,
    ) -> Self {
        Self {
            organization_declared,
            projects: records(projects, |record| {
                Some(Project {
                    name: record["name"].as_str()?.to_owned(),
                    repositories: record["repositories"]
                        .as_array()?
                        .iter()
                        .filter_map(|repository| repository.as_str().map(str::to_owned))
                        .collect(),
                    branch: record["branch"].as_str()?.to_owned(),
                })
            }),
            agents: records(agents, |record| {
                Some(Agent {
                    name: record["name"].as_str()?.to_owned(),
                    harness: record["harness"].as_str()?.to_owned(),
                    model: record["model"].as_str().map(str::to_owned),
                })
            }),
            credentials: records(credentials, |record| {
                record["variable"].as_str().map(str::to_owned)
            }),
        }
    }
}

fn records<T>(records: &Value, read: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    records
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(read)
        .collect()
}

pub struct Project {
    pub name: String,
    pub repositories: Vec<String>,
    pub branch: String,
}

pub struct Agent {
    pub name: String,
    pub harness: String,
    pub model: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Inferred<T> {
    pub value: T,
    pub because: String,
    pub given: bool,
}

fn flagged<T>(value: T, flag: &str) -> Inferred<T> {
    Inferred {
        value,
        because: format!("given as {flag}"),
        given: true,
    }
}

fn inferred<T>(value: T, because: impl Into<String>) -> Inferred<T> {
    Inferred {
        value,
        because: because.into(),
        given: false,
    }
}

pub struct Explained<'a> {
    pub what: &'static str,
    pub value: String,
    pub because: &'a str,
    pub given: bool,
    pub flag: &'static str,
}

pub struct Plan {
    pub organization: Inferred<String>,
    pub repositories: Inferred<Vec<String>>,
    pub project: Inferred<String>,
    pub branch: Inferred<String>,
    pub agent: Inferred<String>,
    pub harness: Inferred<String>,
    pub model: Inferred<Option<String>>,
    pub credentials: Inferred<Vec<String>>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Missing {
    pub flag: &'static str,
    pub because: String,
}

impl LocalClone {
    pub fn of(directory: &Path) -> Self {
        let git = |arguments: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(arguments)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|said| said.trim().to_owned())
                .filter(|said| !said.is_empty())
        };

        let Some(root) = git(&["rev-parse", "--show-toplevel"]) else {
            return Self::default();
        };
        Self {
            root: Some(PathBuf::from(root)),
            origin: git(&["remote", "get-url", "origin"]),
            default_branch: git(&[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ])
            .map(|branch| {
                branch
                    .strip_prefix("origin/")
                    .map_or(branch.clone(), str::to_owned)
            }),
            checked_out: git(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
        }
    }
}

/// Only while no Organization exists is one named for the clone: with one, it is the scope,
/// and with several, guessing would land work in an Organization nobody named (ADR-0016).
pub fn organization(
    named: Option<Scope>,
    existing: &[String],
    clone: &LocalClone,
) -> Result<Inferred<String>, Missing> {
    if let Some(scope) = named {
        return Ok(match &scope.source {
            Source::Flag => flagged(scope.organization, "--organization"),
            Source::Environment => flagged(scope.organization, crate::ORGANIZATION_VARIABLE),
            Source::Binding(binding) => {
                let because = format!("bound by {}", binding.display());
                inferred(scope.organization, because)
            }
            Source::OnlyOrganization => inferred(scope.organization, "the only Organization"),
        });
    }
    match existing {
        [] => Ok(match clone.origin.as_deref() {
            Some(origin) => match owner_and_name(origin).0 {
                Some(owner) => inferred(owner.to_owned(), format!("the owner of origin {origin}")),
                None => inferred(
                    DEFAULT_ORGANIZATION.to_owned(),
                    format!("none exists, and origin {origin} names no owner"),
                ),
            },
            None => inferred(DEFAULT_ORGANIZATION.to_owned(), "none exists yet"),
        }),
        several => Err(Missing {
            flag: "--organization",
            because: format!(
                "{} Organizations exist, and none is in scope: {}",
                several.len(),
                several.join(", ")
            ),
        }),
    }
}

/// Every value past the Organization, which is resolved first because what exists in it is
/// evidence for the rest.
pub fn plan(
    organization: Inferred<String>,
    given: Given,
    clone: &LocalClone,
    existing: &Existing,
) -> Result<Plan, Vec<Missing>> {
    let Existing {
        projects, agents, ..
    } = existing;
    let mut missing = Vec::new();

    let named_project = given
        .project
        .as_ref()
        .and_then(|name| projects.iter().find(|declared| &declared.name == name));
    let repositories = if !given.repositories.is_empty() {
        Some(flagged(given.repositories, "--repository"))
    } else if let Some(named) = named_project {
        Some(inferred(
            named.repositories.clone(),
            format!("what {} is declared against", named.name),
        ))
    } else {
        match (&clone.root, &clone.origin) {
            (Some(root), Some(origin)) => Some(inferred(
                vec![origin.clone()],
                format!("origin of the clone at {}", root.display()),
            )),
            (Some(root), None) => {
                missing.push(Missing {
                    flag: "--repository",
                    because: format!("the clone at {} has no origin remote", root.display()),
                });
                None
            }
            (None, _) => {
                missing.push(Missing {
                    flag: "--repository",
                    because: "the working directory is in no git clone".to_owned(),
                });
                None
            }
        }
    };

    let declaring = repositories.as_ref().and_then(|repositories| {
        projects
            .iter()
            .find(|project| project.repositories == repositories.value)
    });
    let project = match (given.project, declaring, &repositories) {
        (Some(project), _, _) => Some(flagged(project, "--project")),
        (None, Some(declaring), Some(repositories)) => Some(inferred(
            declaring.name.clone(),
            format!("already declares {}", repositories.value.join(", ")),
        )),
        (None, None, Some(repositories)) => {
            let name = owner_and_name(&repositories.value[0]).1;
            if projects.iter().any(|declared| declared.name == name) {
                missing.push(Missing {
                    flag: "--project",
                    because: format!(
                        "the project {name} is declared against other repositories, and a \
                         start changes no declaration"
                    ),
                });
            }
            Some(inferred(name.to_owned(), "named for its repository"))
        }
        (None, _, None) => None,
    };
    let named = project.as_ref().and_then(|project| {
        projects
            .iter()
            .find(|declared| declared.name == project.value)
    });

    let branch = match (given.branch, named) {
        (Some(branch), _) => Some(flagged(branch, "--branch")),
        (None, Some(named)) => Some(inferred(
            named.branch.clone(),
            format!("the branch {} works on", named.name),
        )),
        (None, None) => match (&clone.default_branch, &clone.checked_out) {
            (Some(branch), _) => Some(inferred(branch.clone(), "origin's default branch")),
            (None, Some(branch)) => Some(inferred(
                branch.clone(),
                "checked out, and origin names no default branch",
            )),
            (None, None) => {
                missing.push(Missing {
                    flag: "--branch",
                    because: "origin names no default branch and no branch is checked out"
                        .to_owned(),
                });
                None
            }
        },
    };

    let harness_or_model_given = given.harness.is_some() || given.model.is_some();
    let agent = match (given.agent, agents.as_slice()) {
        (Some(agent), _) => flagged(agent, "--agent"),
        (None, [only]) if !harness_or_model_given => inferred(
            only.name.clone(),
            format!("the only Agent in {}", organization.value),
        ),
        (None, _) => inferred(
            given
                .harness
                .clone()
                .unwrap_or_else(|| DEFAULT_HARNESS.to_owned()),
            "named for its Harness",
        ),
    };
    let declared = agents.iter().find(|declared| declared.name == agent.value);
    let differs = declared.is_some_and(|declared| {
        given
            .harness
            .as_ref()
            .is_some_and(|harness| harness != &declared.harness)
            || given
                .model
                .as_ref()
                .is_some_and(|model| Some(model) != declared.model.as_ref())
    });
    if differs && !agent.given {
        missing.push(Missing {
            flag: "--agent",
            because: format!(
                "the agent {} is declared differently, and a start changes no declaration",
                agent.value
            ),
        });
    }
    let harness = match (given.harness, declared) {
        (Some(harness), _) => flagged(harness, "--harness"),
        (None, Some(declared)) => inferred(
            declared.harness.clone(),
            format!("the harness {} is declared on", declared.name),
        ),
        (None, None) => inferred(DEFAULT_HARNESS.to_owned(), "kestrel's default Harness"),
    };
    let model = match (given.model, declared) {
        (Some(model), _) => flagged(Some(model), "--model"),
        (None, Some(declared)) => inferred(
            declared.model.clone(),
            format!("the model {} is declared with", declared.name),
        ),
        (None, None) => inferred(None, "none, so the Harness chooses"),
    };
    let credentials = match (given.credentials, existing.credentials.as_slice()) {
        (given, held) if !given.is_empty() => {
            let replaced: Vec<&str> = given
                .iter()
                .filter(|variable| held.contains(variable))
                .map(String::as_str)
                .collect();
            let mut credentials = flagged(given.clone(), "--credential");
            if !replaced.is_empty() {
                credentials.because = format!(
                    "given as --credential, replacing what {} holds as {}",
                    organization.value,
                    replaced.join(", ")
                );
            }
            credentials
        }
        (_, []) => inferred(
            Vec::new(),
            "none is held, so a Run reaches a model only through a harness logged in otherwise",
        ),
        (_, held) => inferred(
            held.to_vec(),
            format!("already held by {}", organization.value),
        ),
    };

    match (repositories, project, branch) {
        (Some(repositories), Some(project), Some(branch)) if missing.is_empty() => Ok(Plan {
            organization,
            repositories,
            project,
            branch,
            agent,
            harness,
            model,
            credentials,
        }),
        _ => Err(missing),
    }
}

impl Plan {
    pub fn explained(&self) -> Vec<Explained<'_>> {
        fn row<'a, T>(
            what: &'static str,
            inferred: &'a Inferred<T>,
            value: String,
            flag: &'static str,
        ) -> Explained<'a> {
            Explained {
                what,
                value,
                because: &inferred.because,
                given: inferred.given,
                flag,
            }
        }
        let listed = |values: &[String]| {
            if values.is_empty() {
                "none".to_owned()
            } else {
                values.join(", ")
            }
        };

        vec![
            row(
                "organization",
                &self.organization,
                self.organization.value.clone(),
                "--organization",
            ),
            row(
                "project",
                &self.project,
                self.project.value.clone(),
                "--project",
            ),
            row(
                "repository",
                &self.repositories,
                listed(&self.repositories.value),
                "--repository",
            ),
            row(
                "branch",
                &self.branch,
                self.branch.value.clone(),
                "--branch",
            ),
            row("agent", &self.agent, self.agent.value.clone(), "--agent"),
            row(
                "harness",
                &self.harness,
                self.harness.value.clone(),
                "--harness",
            ),
            row(
                "model",
                &self.model,
                self.model
                    .value
                    .clone()
                    .unwrap_or_else(|| "none".to_owned()),
                "--model",
            ),
            row(
                "credentials",
                &self.credentials,
                listed(&self.credentials.value),
                "--credential",
            ),
        ]
    }

    pub fn applying(&self, existing: &Existing) -> Vec<String> {
        let organization = &self.organization.value;
        let project = &self.project.value;
        let agent = &self.agent.value;
        let mut applying = Vec::new();

        if !existing.organization_declared {
            applying.push(format!(
                "declare the Organization {organization}, the boundary everything below belongs to"
            ));
        }
        if !existing
            .projects
            .iter()
            .any(|declared| &declared.name == project)
        {
            applying.push(format!(
                "declare the Project {project}, where work on {} happens on {}",
                self.repositories.value.join(", "),
                self.branch.value
            ));
        }
        if !existing
            .agents
            .iter()
            .any(|declared| &declared.name == agent)
        {
            let model = self
                .model
                .value
                .as_deref()
                .map_or("its default model".to_owned(), |model| {
                    format!("the model {model}")
                });
            applying.push(format!(
                "declare the Agent {agent}, an actor driven by the Harness {} with {model}",
                self.harness.value
            ));
        }
        if self.credentials.given {
            applying.extend(self.credentials.value.iter().map(|variable| {
                format!("hold {variable} as a Provider Credential of {organization}")
            }));
        }
        applying.push(format!(
            "open a Session in {project} carrying the Brief, and enqueue a Run of {agent} to \
             work on it"
        ));

        applying
    }

    /// Only what was given is sent: what the Organization already holds stays where it is.
    pub fn body(&self, brief: &str, secrets: &[(String, String)]) -> Value {
        json!({
            "organization": self.organization.value,
            "project": {
                "name": self.project.value,
                "repositories": self.repositories.value,
                "branch": self.branch.value,
            },
            "agent": {
                "name": self.agent.value,
                "harness": self.harness.value,
                "model": self.model.value,
            },
            "brief": brief,
            "credentials": secrets
                .iter()
                .map(|(variable, secret)| json!({ "variable": variable, "secret": secret }))
                .collect::<Vec<_>>(),
        })
    }
}

/// Out of the Client's own environment, so a secret is never in a shell history or in what
/// `ps` shows of this process.
pub fn secrets(
    variables: &[String],
    read: impl Fn(&str) -> Option<String>,
) -> Result<Vec<(String, String)>, Vec<Missing>> {
    let mut missing = Vec::new();
    let mut secrets = Vec::new();
    for variable in variables {
        match read(variable).filter(|secret| !secret.is_empty()) {
            Some(secret) => secrets.push((variable.clone(), secret)),
            None => missing.push(Missing {
                flag: "--credential",
                because: format!("{variable} is not set in this environment"),
            }),
        }
    }

    if missing.is_empty() {
        Ok(secrets)
    } else {
        Err(missing)
    }
}

/// A repository on this machine has no owner: its parent directory owns nothing.
fn owner_and_name(url: &str) -> (Option<&str>, &str) {
    let trimmed = url.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let hosted = match trimmed.split_once("://") {
        Some(("file", _)) => None,
        Some((_, rest)) => Some(rest.split_once('/').map_or("", |(_, path)| path)),
        None => trimmed
            .split_once(':')
            .filter(|(host, _)| !host.is_empty() && !host.contains('/'))
            .map(|(_, path)| path),
    };
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);

    let owner = hosted.and_then(|path| {
        let mut segments = path.rsplit('/').filter(|segment| !segment.is_empty());
        segments.next();
        segments.next()
    });

    (owner, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_clone(origin: &str) -> LocalClone {
        LocalClone {
            root: Some(PathBuf::from("/work/widgets")),
            origin: Some(origin.to_owned()),
            default_branch: Some("main".to_owned()),
            checked_out: Some("feature".to_owned()),
        }
    }

    fn acme() -> Inferred<String> {
        inferred("acme".to_owned(), "the owner of origin")
    }

    fn planned(given: Given, clone: &LocalClone, existing: &Existing) -> Plan {
        plan(acme(), given, clone, existing).unwrap_or_else(|missing| {
            panic!("the plan is missing {missing:?}");
        })
    }

    #[test]
    fn an_owner_is_read_from_every_hosted_spelling_and_never_from_a_path() {
        for (url, owner, name) in [
            (
                "https://github.com/acme/widgets.git",
                Some("acme"),
                "widgets",
            ),
            ("https://github.com/acme/widgets/", Some("acme"), "widgets"),
            ("git@github.com:acme/widgets.git", Some("acme"), "widgets"),
            (
                "ssh://git@host:22/group/acme/widgets",
                Some("acme"),
                "widgets",
            ),
            ("file:///srv/git/widgets", None, "widgets"),
            ("/srv/git/widgets.git", None, "widgets"),
        ] {
            assert_eq!(owner_and_name(url), (owner, name), "{url}");
        }
    }

    #[test]
    fn with_no_organization_one_is_named_for_origins_owner() {
        let named = organization(None, &[], &a_clone("https://github.com/acme/widgets.git"));

        assert_eq!(
            named,
            Ok(inferred(
                "acme".to_owned(),
                "the owner of origin https://github.com/acme/widgets.git"
            ))
        );
    }

    #[test]
    fn with_no_organization_and_no_owner_the_default_is_named() {
        let named =
            organization(None, &[], &a_clone("file:///srv/git/widgets")).expect("an organization");

        assert_eq!(named.value, "default");
    }

    #[test]
    fn with_several_organizations_and_none_named_the_flag_is_missing() {
        let named = organization(
            None,
            &["acme".to_owned(), "globex".to_owned()],
            &a_clone("https://github.com/acme/widgets.git"),
        );

        assert_eq!(named.map_err(|missing| missing.flag), Err("--organization"));
    }

    #[test]
    fn a_fresh_clone_infers_every_value_with_a_reason() {
        let plan = planned(
            Given::default(),
            &a_clone("https://github.com/acme/widgets.git"),
            &Existing::default(),
        );

        let explained: Vec<(&str, String, &str)> = plan
            .explained()
            .into_iter()
            .map(|row| (row.what, row.value, row.because))
            .collect();
        assert_eq!(
            explained,
            [
                ("organization", "acme".to_owned(), "the owner of origin"),
                ("project", "widgets".to_owned(), "named for its repository"),
                (
                    "repository",
                    "https://github.com/acme/widgets.git".to_owned(),
                    "origin of the clone at /work/widgets",
                ),
                ("branch", "main".to_owned(), "origin's default branch"),
                ("agent", "opencode".to_owned(), "named for its Harness"),
                (
                    "harness",
                    "opencode".to_owned(),
                    "kestrel's default Harness"
                ),
                ("model", "none".to_owned(), "none, so the Harness chooses"),
                (
                    "credentials",
                    "none".to_owned(),
                    "none is held, so a Run reaches a model only through a harness logged in \
                     otherwise",
                ),
            ]
        );
    }

    #[test]
    fn every_flag_overrides_what_would_be_inferred() {
        let plan = planned(
            Given {
                repositories: vec!["https://github.com/globex/gadgets".to_owned()],
                branch: Some("develop".to_owned()),
                project: Some("gadgets-work".to_owned()),
                agent: Some("builder".to_owned()),
                harness: Some("claude".to_owned()),
                model: Some("opus".to_owned()),
                credentials: vec!["ANTHROPIC_API_KEY".to_owned()],
            },
            &a_clone("https://github.com/acme/widgets.git"),
            &Existing::default(),
        );

        for row in plan.explained().into_iter().skip(1) {
            assert!(row.given, "{}", row.what);
            assert_eq!(row.because, format!("given as {}", row.flag));
        }
        let body = plan.body("go", &[]);
        assert_eq!(body["agent"]["model"], "opus");
        assert_eq!(body["project"]["branch"], "develop");
    }

    #[test]
    fn a_project_already_declaring_the_repository_is_the_one_used() {
        let plan = planned(
            Given::default(),
            &a_clone("https://github.com/acme/widgets.git"),
            &Existing {
                projects: vec![Project {
                    name: "widgets-main".to_owned(),
                    repositories: vec!["https://github.com/acme/widgets.git".to_owned()],
                    branch: "trunk".to_owned(),
                }],
                ..Existing::default()
            },
        );

        assert_eq!(plan.project.value, "widgets-main");
        assert_eq!(plan.branch.value, "trunk");
    }

    #[test]
    fn the_only_agent_is_the_one_used_with_what_it_is_declared_with() {
        let plan = planned(
            Given::default(),
            &a_clone("https://github.com/acme/widgets.git"),
            &Existing {
                agents: vec![Agent {
                    name: "builder".to_owned(),
                    harness: "claude".to_owned(),
                    model: Some("opus".to_owned()),
                }],
                ..Existing::default()
            },
        );

        assert_eq!(plan.agent.value, "builder");
        assert_eq!(plan.harness.value, "claude");
        assert_eq!(plan.model.value.as_deref(), Some("opus"));
    }

    #[test]
    fn with_no_origin_default_the_checked_out_branch_is_used() {
        let plan = planned(
            Given::default(),
            &LocalClone {
                default_branch: None,
                ..a_clone("https://github.com/acme/widgets.git")
            },
            &Existing::default(),
        );

        assert_eq!(plan.branch.value, "feature");
    }

    #[test]
    fn outside_a_clone_each_value_nothing_infers_names_its_flag() {
        let missing = plan(
            acme(),
            Given::default(),
            &LocalClone::default(),
            &Existing::default(),
        )
        .err()
        .expect("the plan is incomplete");

        let flags: Vec<&str> = missing.iter().map(|missing| missing.flag).collect();
        assert_eq!(flags, ["--repository", "--branch"]);
    }

    #[test]
    fn a_credential_given_is_read_from_the_environment_and_never_from_the_command_line() {
        let read = secrets(
            &["ANTHROPIC_API_KEY".to_owned(), "OPENAI_API_KEY".to_owned()],
            |variable| (variable == "ANTHROPIC_API_KEY").then(|| "sk-ant".to_owned()),
        );

        assert_eq!(
            read.map_err(|missing| missing
                .into_iter()
                .map(|missing| missing.because)
                .collect::<Vec<_>>()),
            Err(vec![
                "OPENAI_API_KEY is not set in this environment".to_owned()
            ])
        );
    }

    #[test]
    fn credentials_the_organization_already_holds_are_explained_when_none_is_given() {
        let plan = planned(
            Given::default(),
            &a_clone("https://github.com/acme/widgets.git"),
            &Existing {
                credentials: vec!["ANTHROPIC_API_KEY".to_owned()],
                ..Existing::default()
            },
        );

        assert_eq!(
            plan.credentials,
            inferred(vec!["ANTHROPIC_API_KEY".to_owned()], "already held by acme")
        );
    }

    fn widgets_declared_against(repository: &str) -> Project {
        Project {
            name: "widgets".to_owned(),
            repositories: vec![repository.to_owned()],
            branch: "main".to_owned(),
        }
    }

    #[test]
    fn a_project_named_by_flag_brings_its_own_repositories() {
        let plan = planned(
            Given {
                project: Some("widgets".to_owned()),
                ..Given::default()
            },
            &a_clone("https://github.com/acme/widgets.git"),
            &Existing {
                projects: vec![widgets_declared_against("https://github.com/acme/other")],
                ..Existing::default()
            },
        );

        assert_eq!(plan.repositories.value, ["https://github.com/acme/other"]);
    }

    #[test]
    fn an_inferred_project_name_declared_otherwise_names_the_flag_to_pass() {
        let missing = plan(
            acme(),
            Given::default(),
            &a_clone("https://github.com/acme/widgets.git"),
            &Existing {
                projects: vec![widgets_declared_against("https://github.com/acme/other")],
                ..Existing::default()
            },
        )
        .err()
        .expect("the plan is refused");

        assert_eq!(missing[0].flag, "--project");
    }

    #[test]
    fn an_inferred_agent_declared_otherwise_names_the_flag_to_pass() {
        let missing = plan(
            acme(),
            Given {
                model: Some("sonnet".to_owned()),
                ..Given::default()
            },
            &a_clone("https://github.com/acme/widgets.git"),
            &Existing {
                agents: vec![Agent {
                    name: "opencode".to_owned(),
                    harness: "opencode".to_owned(),
                    model: Some("opus".to_owned()),
                }],
                ..Existing::default()
            },
        )
        .err()
        .expect("the plan is refused");

        assert_eq!(missing[0].flag, "--agent");
    }

    #[test]
    fn applying_a_plan_to_nothing_declares_everything_before_the_run() {
        let plan = planned(
            Given {
                credentials: vec!["ANTHROPIC_API_KEY".to_owned()],
                ..Given::default()
            },
            &a_clone("https://github.com/acme/widgets.git"),
            &Existing::default(),
        );

        assert_eq!(
            plan.applying(&Existing::default()),
            [
                "declare the Organization acme, the boundary everything below belongs to",
                "declare the Project widgets, where work on \
                 https://github.com/acme/widgets.git happens on main",
                "declare the Agent opencode, an actor driven by the Harness opencode \
                 with its default model",
                "hold ANTHROPIC_API_KEY as a Provider Credential of acme",
                "open a Session in widgets carrying the Brief, and enqueue a Run of opencode \
                 to work on it",
            ]
        );
    }

    #[test]
    fn applying_a_plan_redeclares_nothing_that_exists() {
        let existing = Existing {
            organization_declared: true,
            projects: vec![widgets_declared_against(
                "https://github.com/acme/widgets.git",
            )],
            agents: vec![Agent {
                name: "builder".to_owned(),
                harness: "claude".to_owned(),
                model: Some("opus".to_owned()),
            }],
            credentials: vec!["ANTHROPIC_API_KEY".to_owned()],
        };
        let plan = planned(
            Given::default(),
            &a_clone("https://github.com/acme/widgets.git"),
            &existing,
        );

        assert_eq!(
            plan.applying(&existing),
            [
                "open a Session in widgets carrying the Brief, and enqueue a Run of builder to \
              work on it"
            ]
        );
    }

    #[test]
    fn a_credential_given_says_which_held_one_it_replaces() {
        let plan = planned(
            Given {
                credentials: vec!["ANTHROPIC_API_KEY".to_owned()],
                ..Given::default()
            },
            &a_clone("https://github.com/acme/widgets.git"),
            &Existing {
                credentials: vec!["ANTHROPIC_API_KEY".to_owned()],
                ..Existing::default()
            },
        );

        assert!(plan.credentials.given);
        assert_eq!(
            plan.credentials.because,
            "given as --credential, replacing what acme holds as ANTHROPIC_API_KEY"
        );
    }
}
