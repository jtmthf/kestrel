use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::declaration::{self, sharing_a_directory};
use crate::declined::Declined;
use crate::domain::{Run, Session};
use crate::fanout::{self, Change};
use crate::log::Entry;
use crate::provider;
use crate::store::session::Opening;
use crate::store::{Declared, Store};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub organization: String,
    pub project: declaration::Project,
    pub agent: declaration::Agent,
    pub brief: String,
    #[serde(default)]
    pub credentials: Vec<Credential>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub variable: String,
    pub secret: String,
}

pub struct Started {
    pub organization: Settled,
    pub project: Settled,
    pub agent: Settled,
    pub session: Session,
    pub run: Run,
}

#[derive(Serialize)]
pub struct Settled {
    pub name: String,
    pub created: bool,
}

/// A start only adds declarations and refuses one that would change, because the operator asked
/// for work rather than a redeclaration.
pub async fn start(store: &Store, plan: &Plan) -> Result<Started> {
    checked(plan)?;
    let model = plan
        .agent
        .model
        .as_deref()
        .filter(|model| !model.is_empty());

    let mut tx = store.begin().await?;
    // Redeclaring an Organization would clear the live Instance limit it may carry.
    let organization = match tx.organizations().find(&plan.organization).await? {
        Some(record) => Declared {
            record,
            created: false,
        },
        None => tx.organizations().declare(&plan.organization, None).await?,
    };
    for credential in &plan.credentials {
        tx.organizations()
            .hold_provider_credential(
                organization.record.id,
                &credential.variable,
                &credential.secret,
            )
            .await?;
    }

    let found = tx
        .projects()
        .find(&organization.record, &plan.project.name)
        .await?;
    if let Some(found) = found.filter(|found| {
        found.repositories != plan.project.repositories || found.branch != plan.project.branch
    }) {
        return Err(Declined::Taken(format!(
            "the project {} is declared against {} on {}, and a start changes no declaration",
            found.name,
            found.repositories.join(", "),
            found.branch
        ))
        .into());
    }
    let project = tx
        .projects()
        .declare(
            &organization.record,
            &plan.project.name,
            &plan.project.repositories,
            &plan.project.branch,
        )
        .await?;

    let found = tx
        .agents()
        .find(&organization.record, &plan.agent.name)
        .await?;
    if let Some(found) =
        found.filter(|found| found.runtime != plan.agent.runtime || found.model.as_deref() != model)
    {
        return Err(Declined::Taken(format!(
            "the agent {} is declared on the runtime {} with the model {}, and a start changes \
             no declaration",
            found.name,
            found.runtime,
            found.model.as_deref().unwrap_or("its runtime's default")
        ))
        .into());
    }
    let agent = tx
        .agents()
        .declare(
            &organization.record,
            &plan.agent.name,
            &plan.agent.runtime,
            model,
        )
        .await?;

    let session = tx
        .sessions()
        .open(Opening {
            organization: &organization.record,
            project: &project.record,
            agent: &agent.record,
            profile: None,
            branch: None,
            correlation: None,
            continues: None,
            started_by: None,
        })
        .await?;
    tx.log()
        .append(
            &session,
            Entry::Brief {
                trigger: None,
                brief: plan.brief.clone(),
            },
        )
        .await?;
    tx.log()
        .append(
            &session,
            Entry::ParticipantJoined {
                participant: agent.record.name.clone(),
            },
        )
        .await?;
    let run = tx.sessions().enqueue_run(&session, None).await?;
    tx.commit().await?;
    fanout::publish(Change::SessionOpened(&session));

    Ok(Started {
        organization: Settled {
            name: organization.record.name,
            created: organization.created,
        },
        project: Settled {
            name: project.record.name,
            created: project.created,
        },
        agent: Settled {
            name: agent.record.name,
            created: agent.created,
        },
        session,
        run,
    })
}

fn checked(plan: &Plan) -> Result<()> {
    for credential in &plan.credentials {
        provider::holdable(&credential.variable, &credential.secret)?;
    }
    let unacceptable = |why: &str| Err(Declined::Unacceptable(why.to_owned()).into());
    if [&plan.organization, &plan.project.name, &plan.agent.name]
        .iter()
        .any(|name| name.is_empty())
    {
        return unacceptable("a start names its organization, project and agent");
    }
    if plan.project.repositories.is_empty() {
        return unacceptable("a project names at least one repository");
    }
    if let Some(clash) = sharing_a_directory(&plan.project.repositories) {
        return Err(Declined::Unacceptable(clash).into());
    }
    if plan.project.branch.is_empty() {
        return unacceptable("a project names the branch its work happens on");
    }
    if plan.agent.runtime.is_empty() {
        return unacceptable("an agent names the agent runtime that drives it");
    }
    if plan.brief.trim().is_empty() {
        return unacceptable("a start carries a brief");
    }

    Ok(())
}
