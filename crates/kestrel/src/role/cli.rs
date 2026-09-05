use anyhow::Result;

use crate::cli::{
    AgentCommand, CliCommand, OrganizationCommand, RunCommand, SessionCommand, WorkspaceCommand,
};
use crate::log::Window;
use crate::session;
use crate::store::Store;
use crate::work;

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
            continues,
        }) => {
            let session = session::open(&store, organization, workspace, agent, *continues).await?;
            println!("{}", session.id);
        }
        CliCommand::Session(SessionCommand::Seal { session }) => {
            let sealed = session::seal(&store, *session).await?;
            println!("{}", sealed.id);
        }
        CliCommand::Session(SessionCommand::Show { session }) => {
            let session = session::show(&store, *session).await?;
            println!("session       {}", session.id);
            println!("organization  {}", session.organization.name);
            println!("workspace     {}", session.workspace.name);
            println!("agent         {}", session.agent.name);
            println!("state         {}", session.state);
            println!("opened        {}", session.opened_at);
            if let Some(sealed_at) = session.sealed_at {
                println!("sealed        {sealed_at}");
            }
            if let Some(continues) = session.continues {
                println!("continues     {continues}");
            }
            for continuation in session::continuations(&store, session.id).await? {
                println!("continued-by  {continuation}");
            }
        }
        CliCommand::Session(SessionCommand::Transcript {
            session,
            cursor,
            window,
        }) => {
            let window = Window::or_default(*window)?;
            let page = session::transcript(&store, *session, *cursor, window).await?;

            for entry in &page.entries {
                println!("{}  {}  {}", entry.seq, entry.appended_at, entry.entry);
            }
            // Beside the Transcript rather than in it: an entry carrying a line of its own
            // that reads `cursor  …` would otherwise be indistinguishable from this one.
            if let Some(cursor) = page.cursor {
                eprintln!("cursor  {cursor}");
            }
        }
        CliCommand::Run(RunCommand::Enqueue { session }) => {
            let run = work::enqueue(&store, *session).await?;
            println!("{}", run.id);
        }
        CliCommand::Run(RunCommand::List { session }) => {
            for run in work::runs(&store, *session).await? {
                println!(
                    "{}  {}  {}",
                    run.id,
                    run.environment.as_deref().unwrap_or("-"),
                    run.exit
                        .map_or_else(|| run.state.to_string(), |exit| exit.to_string())
                );
            }
        }
    }

    Ok(())
}
