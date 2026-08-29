use anyhow::Result;

use crate::cli::{AgentCommand, CliCommand, OrganizationCommand, SessionCommand, WorkspaceCommand};
use crate::session;
use crate::store::Store;

pub async fn run(command: &CliCommand, store: Store) -> Result<()> {
    match command {
        CliCommand::Organization(OrganizationCommand::Declare { name }) => {
            let mut tx = store.begin().await?;
            let organization = tx.declare_organization(name).await?;
            tx.commit().await?;
            println!("{}", organization.id);
        }
        CliCommand::Organization(OrganizationCommand::List) => {
            let mut tx = store.begin().await?;
            for organization in tx.organizations().await? {
                println!("{}  {}", organization.id, organization.name);
            }
        }
        CliCommand::Workspace(WorkspaceCommand::Declare {
            name,
            organization,
            repositories,
            branch,
        }) => {
            let mut tx = store.begin().await?;
            let organization = tx.organization_named(organization).await?;
            let workspace = tx
                .declare_workspace(&organization, name, repositories, branch)
                .await?;
            tx.commit().await?;
            println!("{}", workspace.id);
        }
        CliCommand::Workspace(WorkspaceCommand::List { organization }) => {
            let mut tx = store.begin().await?;
            let organization = tx.organization_named(organization).await?;
            for workspace in tx.workspaces(&organization).await? {
                println!(
                    "{}  {}  {}  {}",
                    workspace.id,
                    workspace.name,
                    workspace.branch,
                    workspace.repositories.join(",")
                );
            }
        }
        CliCommand::Agent(AgentCommand::Declare {
            name,
            organization,
            runtime,
            model,
        }) => {
            let mut tx = store.begin().await?;
            let organization = tx.organization_named(organization).await?;
            let agent = tx
                .declare_agent(&organization, name, runtime, model)
                .await?;
            tx.commit().await?;
            println!("{}", agent.id);
        }
        CliCommand::Agent(AgentCommand::List { organization }) => {
            let mut tx = store.begin().await?;
            let organization = tx.organization_named(organization).await?;
            for agent in tx.agents(&organization).await? {
                println!(
                    "{}  {}  {}  {}",
                    agent.id, agent.name, agent.runtime, agent.model
                );
            }
        }
        CliCommand::Session(SessionCommand::Open {
            organization,
            workspace,
            agent,
        }) => {
            let session = session::open(&store, organization, workspace, agent).await?;
            println!("{}", session.id);
        }
        CliCommand::Session(SessionCommand::Show { session }) => {
            let session = session::show(&store, *session).await?;
            println!("session       {}", session.id);
            println!("organization  {}", session.organization.name);
            println!("workspace     {}", session.workspace.name);
            println!("agent         {}", session.agent.name);
            println!("state         {}", session.state);
            println!("opened        {}", session.opened_at);
        }
        CliCommand::Session(SessionCommand::Transcript { session }) => {
            for entry in session::transcript(&store, *session).await? {
                println!("{}  {}  {}", entry.seq, entry.appended_at, entry.entry);
            }
        }
    }

    Ok(())
}
