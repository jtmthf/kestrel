use std::io::Read as _;

use anyhow::{Result, bail};

use crate::agent;
use crate::cli::{
    AgentCommand, CliCommand, CredentialCommand, EventCommand, IntegrationCommand,
    OrganizationCommand, RegisterCommand, RunCommand, SessionCommand, TriggerCommand,
    WorkspaceCommand,
};
use crate::domain::{Direction, IntegrationKind, Templates};
use crate::integration::{self, Registration};
use crate::log::Window;
use crate::provider;
use crate::session;
use crate::store::Store;
use crate::trigger::{self, Declaration};
use crate::work;

pub async fn run(command: &CliCommand, store: Store) -> Result<()> {
    match command {
        CliCommand::Organization(OrganizationCommand::Declare { name }) => {
            let mut tx = store.begin().await?;
            let organization = tx.organizations().declare(name).await?;
            tx.commit().await?;
            println!("{}", organization.id);
        }
        CliCommand::Organization(OrganizationCommand::List) => {
            let mut tx = store.begin().await?;
            for organization in tx.organizations().all().await? {
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
            let organization = tx.organizations().named(organization).await?;
            let workspace = tx
                .workspaces()
                .declare(&organization, name, repositories, branch)
                .await?;
            tx.commit().await?;
            println!("{}", workspace.id);
        }
        CliCommand::Workspace(WorkspaceCommand::List { organization }) => {
            let mut tx = store.begin().await?;
            let organization = tx.organizations().named(organization).await?;
            for workspace in tx.workspaces().all(&organization).await? {
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
            let declared =
                agent::declare(&store, organization, name, runtime, model.as_deref()).await?;
            println!("{}", declared.id);
        }
        CliCommand::Agent(AgentCommand::Model {
            name,
            organization,
            model,
        }) => {
            let changed = agent::set_model(&store, organization, name, model.as_deref()).await?;
            println!("{}", changed.model.as_deref().unwrap_or("-"));
        }
        CliCommand::Agent(AgentCommand::List { organization }) => {
            for agent in agent::agents(&store, organization).await? {
                println!(
                    "{}  {}  {}  {}",
                    agent.id,
                    agent.name,
                    agent.runtime,
                    agent.model.as_deref().unwrap_or("-")
                );
            }
        }
        CliCommand::Credential(CredentialCommand::Set {
            variable,
            organization,
        }) => {
            provider::hold(&store, organization, variable, &read_the_secret()?).await?;
            println!("{variable}");
        }
        CliCommand::Credential(CredentialCommand::List { organization }) => {
            for held in provider::held(&store, organization).await? {
                println!("{}  {}", held.variable, held.set_at);
            }
        }
        CliCommand::Credential(CredentialCommand::Forget {
            variable,
            organization,
        }) => {
            provider::forget(&store, organization, variable).await?;
            println!("{variable}");
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
        CliCommand::Session(SessionCommand::List { organization }) => {
            for session in session::sessions(&store, organization).await? {
                println!(
                    "{}  {}  {}  {}  {}",
                    session.id,
                    session.state,
                    session.workspace.name,
                    session.agent.name,
                    session
                        .started_by
                        .map_or_else(|| "-".to_owned(), |event| event.to_string())
                );
            }
        }
        CliCommand::Session(SessionCommand::Seal { session }) => {
            let sealed = session::seal(&store, *session).await?;
            println!("{}", sealed.id);
        }
        CliCommand::Session(SessionCommand::Post {
            session,
            as_participant,
            message,
        }) => {
            let run = session::post(&store, *session, as_participant, message).await?;
            println!(
                "{}",
                run.map_or_else(|| "pending".to_owned(), |run| run.id.to_string())
            );
        }
        CliCommand::Session(SessionCommand::Show { session }) => {
            let session = session::show(&store, *session).await?;
            println!("session       {}", session.id);
            println!("organization  {}", session.organization.name);
            println!("workspace     {}", session.workspace.name);
            println!("agent         {}", session.agent.name);
            println!("branch        {}", session.branch);
            if let Some(correlation) = &session.correlation {
                println!("correlation   {correlation}");
            }
            println!("state         {}", session.state);
            println!("opened        {}", session.opened_at);
            println!("last active   {}", session.last_active_at);
            if let Some(sealed_at) = session.sealed_at {
                println!("sealed        {sealed_at}");
            }
            if let Some(event) = session::started_by(&store, &session).await? {
                println!(
                    "event         {}  {}",
                    event.record_id, event.occurrence.source
                );
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
        CliCommand::Integration(IntegrationCommand::Register(RegisterCommand::Github {
            name,
            organization,
            repository,
            token,
            carries,
            interval,
            api,
        })) => {
            let integration = integration::register(
                &store,
                Registration {
                    organization,
                    name,
                    kind: IntegrationKind::Github,
                    repository,
                    api,
                    token,
                    carries,
                    interval: *interval,
                },
            )
            .await?;
            println!("{}", integration.id);
        }
        CliCommand::Integration(IntegrationCommand::List { organization }) => {
            for integration in integration::integrations(&store, organization).await? {
                println!(
                    "{}  {}  {}  {}  {}  every {:#}",
                    integration.id,
                    integration.name,
                    integration.kind,
                    integration.repository,
                    integration
                        .carries
                        .iter()
                        .copied()
                        .map(Direction::as_str)
                        .collect::<Vec<_>>()
                        .join(","),
                    integration.interval
                );
                if let Some(refusal) = integration.last_event_refusal {
                    println!(
                        "  refused {} {} ({} bytes) at {}: {}",
                        refusal.source,
                        refusal.id,
                        refusal.bytes,
                        refusal.observed_at,
                        refusal.reason
                    );
                }
            }
        }
        CliCommand::Integration(IntegrationCommand::AcknowledgeRefusal { name, organization }) => {
            integration::acknowledge_event_refusal(&store, organization, name).await?;
            println!("acknowledged");
        }
        CliCommand::Trigger(TriggerCommand::Declare {
            name,
            organization,
            filter,
            brief,
            branch,
            correlation,
            workspace,
            agent,
        }) => {
            let trigger = trigger::declare(
                &store,
                Declaration {
                    organization,
                    name,
                    filter,
                    templates: &Templates {
                        brief: brief.clone(),
                        branch: branch.clone(),
                        correlation: correlation.clone(),
                    },
                    workspace,
                    agent,
                },
            )
            .await?;
            println!("{}", trigger.id);
        }
        CliCommand::Trigger(TriggerCommand::List { organization }) => {
            for trigger in trigger::triggers(&store, organization).await? {
                println!(
                    "{}  {}  {}  {}  {}  {}",
                    trigger.id,
                    trigger.name,
                    trigger.state,
                    trigger.workspace.name,
                    trigger.agent.name,
                    trigger.filter
                );
            }
        }
        CliCommand::Trigger(TriggerCommand::Test {
            name,
            organization,
            event,
        }) => {
            let tested = trigger::test(&store, organization, name, *event).await?;
            if tested.matches {
                println!("matches");
            } else {
                println!("does not match");
            }

            let rendered = tested.rendered?;
            println!("branch        {}", rendered.branch);
            println!(
                "correlation   {}",
                rendered.correlation.as_deref().unwrap_or("-")
            );
            println!();
            println!("{}", rendered.brief);
        }
        CliCommand::Trigger(TriggerCommand::Show { name, organization }) => {
            let trigger = trigger::show(&store, organization, name).await?;
            let templates = &trigger.templates;
            println!("trigger       {}", trigger.id);
            println!("organization  {}", trigger.organization.name);
            println!("name          {}", trigger.name);
            println!("state         {}", trigger.state);
            println!("matches       {}", trigger.filter);
            println!("workspace     {}", trigger.workspace.name);
            println!("agent         {}", trigger.agent.name);
            match &templates.branch {
                Some(branch) => println!("branch        {branch}"),
                None => println!(
                    "branch        {} (the workspace's)",
                    trigger.workspace.branch
                ),
            }
            println!(
                "correlation   {}",
                templates
                    .correlation
                    .as_ref()
                    .map_or_else(|| "-".to_owned(), ToString::to_string)
            );
            println!("declared      {}", trigger.declared_at);
            println!();
            println!("{}", templates.brief);
        }
        CliCommand::Trigger(TriggerCommand::Disable { name, organization }) => {
            let trigger = trigger::disable(&store, organization, name).await?;
            println!("{}", trigger.state);
        }
        CliCommand::Trigger(TriggerCommand::Enable { name, organization }) => {
            let trigger = trigger::enable(&store, organization, name).await?;
            println!("{}", trigger.state);
        }
        CliCommand::Event(EventCommand::List {
            organization,
            limit,
        }) => {
            for event in integration::events(&store, organization, *limit).await? {
                println!(
                    "record={}  id={}  time={}  source={}  type={}  subject={}  data={}",
                    event.record_id,
                    event.occurrence.id,
                    event.occurrence.time,
                    event.occurrence.source,
                    event.occurrence.r#type,
                    event.occurrence.subject.as_deref().unwrap_or("-"),
                    event.occurrence.data
                );
            }
        }
        CliCommand::Event(EventCommand::Show { record, json }) => {
            let event = integration::event(&store, *record).await?;
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "record": event.record_id,
                        "organization": event.organization,
                        "integration": event.integration,
                        "recorded_at": event.recorded_at,
                        "event": event.occurrence,
                    }))?
                );
                return Ok(());
            }
            println!("record        {}", event.record_id);
            println!("organization  {}", event.organization);
            println!("integration   {}", event.integration);
            println!("id            {}", event.occurrence.id);
            println!("source        {}", event.occurrence.source);
            println!("specversion   {}", event.occurrence.specversion);
            println!("type          {}", event.occurrence.r#type);
            println!(
                "subject       {}",
                event.occurrence.subject.as_deref().unwrap_or("-")
            );
            println!("time          {}", event.occurrence.time);
            println!("recorded      {}", event.recorded_at);
            println!(
                "data          {}",
                serde_json::to_string_pretty(&event.occurrence.data)?
            );
        }
        CliCommand::Run(RunCommand::List { session }) => {
            for run in work::runs(&store, *session).await? {
                println!(
                    "{}  {}  {}  {}",
                    run.id,
                    run.environment.as_deref().unwrap_or("-"),
                    run.model.as_deref().unwrap_or("-"),
                    run.exit
                        .map_or_else(|| run.state.to_string(), |exit| exit.to_string())
                );
            }
        }
    }

    Ok(())
}

/// Off standard input rather than out of an argument, so a provider's key is never in a shell
/// history or in what `ps` shows of this process.
fn read_the_secret() -> Result<String> {
    let mut read = String::new();
    std::io::stdin().read_to_string(&mut read)?;
    let secret = read.trim();

    if secret.is_empty() {
        bail!("a provider credential is read from standard input, and nothing was on it");
    }

    Ok(secret.to_owned())
}
