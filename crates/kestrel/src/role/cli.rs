use std::io::Read as _;

use anyhow::{Context as _, Result, bail};

use crate::agent;
use crate::cli::{
    AgentCommand, CliCommand, CredentialCommand, EventCommand, Given, InstanceCommand,
    IntegrationCommand, OrganizationCommand, RegisterCommand, RunCommand, SessionCommand,
    TriggerCommand, WorkspaceCommand,
};
use crate::domain::{Connection, Direction, Fires, Templates};
use crate::filter::Filter;
use crate::instance;
use crate::integration::{self, Connecting, Registration};
use crate::log::Window;
use crate::provider;
use crate::session;
use crate::store::Store;
use crate::trigger::apply::{self, Action};
use crate::trigger::{self, Declaration};
use crate::work;

pub async fn run(command: &CliCommand, store: Store) -> Result<()> {
    match command {
        CliCommand::Organization(OrganizationCommand::Declare { name }) => {
            let mut tx = store.begin().await?;
            let declared = tx.organizations().declare(name).await?;
            tx.commit().await?;
            println!("{}", declared.record.id);
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
            let declared = tx
                .workspaces()
                .declare(&organization, name, repositories, branch)
                .await?;
            tx.commit().await?;
            println!("{}", declared.record.id);
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
            println!("{}", declared.record.id);
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
            branch,
            continues,
        }) => {
            let session = session::open(
                &store,
                organization,
                workspace,
                agent,
                branch.as_deref(),
                *continues,
            )
            .await?;
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
            println!("runtime       {}", session.agent.runtime);
            println!(
                "model         {}",
                session.agent.model.as_deref().unwrap_or("-")
            );
            println!("branch        {}", session.checkout.branch);
            println!("base          {}", session.checkout.base);
            if let Some(kept) = work::instance(&store, session.id).await? {
                println!("instance      {kept}");
            }
            if let Some(held) = instance::held_by(&store, session.id).await? {
                println!("held          {}", held.because);
            }
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
        CliCommand::Instance(InstanceCommand::List { organization }) => {
            for held in instance::held(&store, organization).await? {
                println!("{}  {}  {}", held.session, held.instance, held.because);
            }
        }
        CliCommand::Instance(InstanceCommand::Release {
            session,
            as_participant,
        }) => {
            println!(
                "{}",
                instance::release(&store, *session, as_participant).await?
            );
        }
        CliCommand::Run(RunCommand::Enqueue { session, model }) => {
            let run = work::enqueue(&store, *session, model.as_deref()).await?;
            println!("{}", run.id);
        }
        CliCommand::Integration(IntegrationCommand::Register(RegisterCommand::Github {
            name,
            organization,
            repository,
            token,
            carries,
            interval,
            webhook_secret,
            api,
        })) => {
            let integration = integration::register(
                &store,
                Registration {
                    organization,
                    name,
                    carries,
                    connecting: Connecting::Github {
                        repository,
                        api,
                        token,
                        interval: *interval,
                        signing_secret: webhook_secret.as_deref(),
                    },
                },
            )
            .await?;
            println!("{}", integration.id);
        }
        CliCommand::Integration(IntegrationCommand::Register(RegisterCommand::Webhook {
            name,
            organization,
            secret,
        })) => {
            let integration = integration::register(
                &store,
                Registration {
                    organization,
                    name,
                    carries: &[Direction::Inbound],
                    connecting: Connecting::Webhook { secret },
                },
            )
            .await?;
            println!("{}", integration.id);
        }
        CliCommand::Integration(IntegrationCommand::List { organization }) => {
            for integration in integration::integrations(&store, organization).await? {
                let (watching, receiving) = match &integration.connection {
                    Connection::Github(github) if github.signed => (
                        github.repository.as_str(),
                        format!("at {}", integration.webhook_path()),
                    ),
                    Connection::Github(github) => (
                        github.repository.as_str(),
                        format!("every {:#}", github.interval),
                    ),
                    Connection::Webhook => ("-", format!("at {}", integration.webhook_path())),
                };
                println!(
                    "{}  {}  {}  {}  {}  {}",
                    integration.id,
                    integration.name,
                    integration.kind(),
                    watching,
                    integration
                        .carries
                        .iter()
                        .copied()
                        .map(Direction::as_str)
                        .collect::<Vec<_>>()
                        .join(","),
                    receiving
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
            every,
            brief,
            branch,
            correlation,
            on_miss,
            workspace,
            agent,
            allows,
        }) => {
            let fires = match (filter, every) {
                (Some(filter), None) => {
                    if *filter == Given::Stdin && *brief == Given::Stdin {
                        bail!("the filter and the brief cannot both be read from standard input");
                    }
                    let filter: Filter = filter.parse("filter")?;
                    if filter.admits_outsiders() {
                        warn_of_outsiders(name);
                    }
                    Fires::On(filter)
                }
                (None, Some(every)) => Fires::Every(*every),
                _ => bail!("a trigger declares a filter or a schedule, and not both"),
            };
            let trigger = trigger::declare(
                &store,
                Declaration {
                    organization,
                    name,
                    fires: &fires,
                    templates: &Templates {
                        brief: brief.parse("brief")?,
                        branch: branch.clone(),
                        correlation: correlation.clone(),
                    },
                    on_miss: *on_miss,
                    workspace,
                    agent,
                    allows,
                },
            )
            .await?;
            println!("{}", trigger.id);
        }
        CliCommand::Trigger(TriggerCommand::Apply {
            organization,
            file,
            dry_run,
        }) => {
            let declarations = apply::parse(&file.read()?)?;
            let applied = apply::apply(&store, organization, &declarations, *dry_run).await?;

            if applied.changes.is_empty() {
                println!("no changes");
            }
            for change in &applied.changes {
                let sign = match change.action {
                    Action::Add => '+',
                    Action::Change => '~',
                    Action::Remove => '-',
                };
                println!("{sign} {}", change.name);
                for difference in &change.differences {
                    println!("    {}", difference.field);
                    for (sign, value) in [('-', &difference.was), ('+', &difference.becomes)] {
                        for line in value.iter().flat_map(|value| value.lines()) {
                            println!("      {sign} {line}");
                        }
                    }
                }
            }
            for name in &applied.admitting_outsiders {
                warn_of_outsiders(name);
            }
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
                    trigger.fires
                );
            }
        }
        CliCommand::Trigger(TriggerCommand::Test {
            name,
            organization,
            event,
            file,
            instruction,
        }) => {
            if *file == Some(Given::Stdin) && *instruction == Some(Given::Stdin) {
                bail!(
                    "the declaration file and the instruction cannot both be read from standard input"
                );
            }
            let instruction = instruction.as_ref().map(Given::read).transpose()?;
            let instruction = instruction.as_deref();
            let tested = match file {
                Some(file) => {
                    let declarations = apply::parse(&file.read()?)?;
                    let declared = declarations
                        .iter()
                        .find(|declared| declared.name == *name)
                        .with_context(|| {
                            format!("the declaration file declares no trigger {name}")
                        })?;
                    trigger::test_declared(&store, organization, declared, *event, instruction)
                        .await?
                }
                None => trigger::test(&store, organization, name, *event, instruction).await?,
            };
            if tested.matches {
                println!("matches");
            } else {
                println!("does not match");
            }
            if let Some(elapsing) = tested.elapsing {
                println!("elapsing      {elapsing}");
            }

            let rendered = tested.rendered?;
            println!("agent         {}", tested.agent?);
            println!(
                "branch        {}",
                rendered.branch.as_deref().unwrap_or("the session's own")
            );
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
            if let Some(because) = &trigger.disabled_because {
                println!("disabled      {because}");
            }
            println!(
                "budget        {} firings in {}",
                trigger.firing_budget.limit, trigger.firing_budget.window
            );
            println!("fires         {}", trigger.fires);
            println!("workspace     {}", trigger.workspace.name);
            println!("agent         {}", trigger.agent.name);
            if !trigger.allows.is_empty() {
                println!(
                    "allows        {}",
                    trigger
                        .allows
                        .iter()
                        .map(|agent| agent.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            match &templates.branch {
                Some(branch) => println!("branch        {branch}"),
                None => println!(
                    "branch        the session's own, cut from {}",
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
            if let Some(on_miss) = trigger.on_miss {
                println!("on miss       {on_miss}");
            }
            println!("declared      {}", trigger.declared_at);
            println!(
                "declared by   {}",
                if trigger.applied { "a file" } else { "flags" }
            );
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
            println!(
                "integration   {}",
                event
                    .integration
                    .map_or_else(|| "-".to_owned(), |integration| integration.to_string())
            );
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
                    run.instance.as_deref().unwrap_or("-"),
                    run.worked_model.as_deref().unwrap_or("-"),
                    run.exit
                        .map_or_else(|| run.state.to_string(), |exit| exit.to_string())
                );
            }
        }
    }

    Ok(())
}

fn warn_of_outsiders(name: &str) {
    eprintln!(
        "warning: the trigger {name} fires for events from people outside the organization. \
         Until 0.4, it is an unsupervised agent with your credentials on your repository, \
         briefed by whatever a stranger writes. Filter on the author_association GitHub \
         reports as OWNER, MEMBER or COLLABORATOR to decline strangers, or keep it as a \
         decision you made on purpose."
    );
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
