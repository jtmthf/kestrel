//! A canned sequence over real stdio JSON-RPC, so the main suite drives kestrel's ACP client
//! over the wire rather than over a shim above it, with no network and no model spend.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, AuthMethod, AuthMethodAgent, AuthMethodTerminal, AvailableCommandsUpdate,
    ContentBlock, ContentChunk, Cost, InitializeRequest, InitializeResponse, MessageId,
    NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionKind, Plan, PlanEntry,
    PlanEntryPriority, PlanEntryStatus, PromptCapabilities, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelect, SessionConfigSelectOption,
    SessionConfigValueId, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, StopReason, TextContent, ToolCall, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, UsageUpdate,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Error, Result, Stdio};
use clap::Parser;
use kestrel_scripted_agent::{
    CONFIDED, DEFAULT_MODEL, FIRST_MEMORY, LAST_MEMORY, LOGIN, MUTTERED, OTHER_MODEL, OVERLONG,
    REFRESHED, Script, conversed,
};

const SESSION: &str = "scripted";
/// Long enough to kill a control plane and bring it back up under a turn that is in flight.
const LINGER: Duration = Duration::from_secs(3);
const TOOL_CALL: &str = "call-1";
const MODEL_OPTION: &str = "model";
const ALLOW_ONCE: &str = "allow-once";

#[derive(Debug, Parser)]
#[command(name = "kestrel-scripted-agent", version)]
struct Cli {
    /// The sequence to play
    #[arg(long, value_enum, default_value = "speaks")]
    script: Script,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let script = Cli::parse().script;
    let prompted_so_far: Arc<Mutex<Vec<String>>> = Arc::default();
    let opened_against: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let located = Arc::clone(&opened_against);

    Agent
        .builder()
        .name("kestrel-scripted-agent")
        .on_receive_request(
            async move |initialize: InitializeRequest, responder, _connection| {
                if initialize.protocol_version != ProtocolVersion::V1 {
                    return responder.respond_with_error(Error::invalid_params().data(format!(
                        "this agent speaks ACP v1, and was initialized with {:?}",
                        initialize.protocol_version
                    )));
                }

                if script == Script::Predates {
                    return responder.respond(InitializeResponse::new(ProtocolVersion::V0));
                }

                if script == Script::Demands {
                    return responder.respond(
                        InitializeResponse::new(ProtocolVersion::V1).auth_methods(vec![
                            AuthMethod::Terminal(AuthMethodTerminal::new(
                                "terminal",
                                "Log in at a terminal",
                            )),
                        ]),
                    );
                }

                if script == Script::Insists {
                    return responder.respond(
                        InitializeResponse::new(ProtocolVersion::V1).auth_methods(vec![
                            AuthMethod::Agent(AuthMethodAgent::new(
                                "its-own",
                                "Log in as the agent asks",
                            )),
                        ]),
                    );
                }

                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                        AgentCapabilities::new().prompt_capabilities(PromptCapabilities::new()),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |new: NewSessionRequest, responder, _connection| {
                *opened_against
                    .lock()
                    .expect("where the session was opened should not be poisoned") = Some(new.cwd);
                if script == Script::Insists {
                    return responder.respond_with_error(Error::auth_required());
                }

                responder.respond(match script {
                    Script::Decides => NewSessionResponse::new(SESSION),
                    _ => {
                        NewSessionResponse::new(SESSION).config_options(vec![models(DEFAULT_MODEL)])
                    }
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |set: SetSessionConfigOptionRequest, responder, _connection| {
                let Some(selected) = set.value.as_value_id() else {
                    return responder.respond_with_error(
                        Error::invalid_params().data("this agent's model is a selection"),
                    );
                };

                responder.respond(SetSessionConfigOptionResponse::new(vec![models(
                    selected.clone(),
                )]))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |prompt: PromptRequest, responder, connection| {
                let prompted_so_far = Arc::clone(&prompted_so_far);
                let located = located
                    .lock()
                    .expect("where the session was opened should not be poisoned")
                    .clone();
                let prompted: String = prompt
                    .prompt
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect();
                // The turn asks the client a question of its own, so it cannot run inside the
                // dispatch loop that would have to carry the answer.
                connection.clone().spawn(async move {
                    let earlier = {
                        let mut so_far = prompted_so_far
                            .lock()
                            .expect("what was prompted should not be poisoned");
                        let earlier = so_far.clone();
                        so_far.push(prompted.clone());
                        earlier
                    };
                    let stop = play(script, &prompted, &earlier, located, &connection).await?;
                    responder.respond(PromptResponse::new(stop))
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new())
        .await
}

async fn play(
    script: Script,
    prompted: &str,
    earlier: &[String],
    located: Option<PathBuf>,
    connection: &ConnectionTo<Client>,
) -> Result<StopReason> {
    if script == Script::Mutters {
        eprintln!("{MUTTERED}");
        eprintln!("{}", "a".repeat(OVERLONG));
    }
    if matches!(script, Script::Dawdles | Script::Mutters) {
        std::future::pending::<()>().await;
    }
    if script == Script::Lingers {
        tokio::time::sleep(LINGER).await;
    }
    if script == Script::Confides {
        say(connection, "message-1", &confided())?;
        return Ok(StopReason::EndTurn);
    }
    if script == Script::Refreshes {
        say(connection, "message-1", &refreshed())?;
        return Ok(StopReason::EndTurn);
    }
    if script == Script::Recalls {
        let remembers = prompted.contains(FIRST_MEMORY) && prompted.contains(LAST_MEMORY);
        let message = match remembers {
            true => "I remember the whole earlier context",
            false => "I forgot part of the earlier context",
        };
        say(connection, "message-1", message)?;
        return Ok(StopReason::EndTurn);
    }
    if script == Script::Locates {
        let said = match located {
            Some(directory) => directory.display().to_string(),
            None => "no session was opened".to_owned(),
        };
        say(connection, "message-1", &said)?;
        return Ok(StopReason::EndTurn);
    }
    if script == Script::Echoes {
        say(connection, "message-1", prompted)?;
        return Ok(StopReason::EndTurn);
    }
    if script == Script::Converses {
        say(
            connection,
            "message-1",
            &conversed(earlier.len() + 1, earlier),
        )?;
        return Ok(StopReason::EndTurn);
    }
    if script == Script::Refuses {
        say(connection, "message-1", "this is not work I will do")?;
        return Ok(StopReason::Refusal);
    }
    if script == Script::Silent {
        update(
            connection,
            SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(Vec::new())),
        )?;
        return Ok(StopReason::EndTurn);
    }
    if script == Script::Works {
        update(
            connection,
            SessionUpdate::ToolCall(
                ToolCall::new(TOOL_CALL, "read README.md").status(ToolCallStatus::Completed),
            ),
        )?;
        return Ok(StopReason::EndTurn);
    }
    if script == Script::Asks {
        permission_to_use_a_tool(connection).await?;
        return Ok(StopReason::EndTurn);
    }
    if script == Script::Lapses {
        if earlier.is_empty() {
            say(connection, "message-1", "the first turn is answered")?;
        }
        return Ok(StopReason::EndTurn);
    }

    update(
        connection,
        SessionUpdate::Plan(Plan::new(vec![PlanEntry::new(
            "read the issue",
            PlanEntryPriority::High,
            PlanEntryStatus::Pending,
        )])),
    )?;
    update(
        connection,
        SessionUpdate::AgentThoughtChunk(chunk(None, "the issue looks small")),
    )?;
    update(
        connection,
        SessionUpdate::ToolCall(
            ToolCall::new(TOOL_CALL, "read README.md").status(ToolCallStatus::Pending),
        ),
    )?;

    permission_to_use_a_tool(connection).await?;

    if script == Script::Dies {
        std::process::exit(9);
    }

    update(
        connection,
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            TOOL_CALL,
            ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
        )),
    )?;

    update(
        connection,
        SessionUpdate::AgentMessageChunk(chunk(Some("message-1"), "half of one message, ")),
    )?;
    update(
        connection,
        SessionUpdate::AgentMessageChunk(chunk(Some("message-1"), "and the other half")),
    )?;
    say(connection, "message-2", "a second message")?;

    update(
        connection,
        SessionUpdate::UsageUpdate(UsageUpdate::new(1_200, 200_000).cost(Cost::new(0.42, "USD"))),
    )?;

    Ok(StopReason::EndTurn)
}

/// The only way a test sees where a credential reached: an agent saying what its own process
/// was spawned with.
fn confided() -> String {
    let mut reached: Vec<String> = std::env::vars()
        .filter(|(name, _)| name.starts_with(CONFIDED))
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    reached.sort();

    match reached.is_empty() {
        true => "nothing reached this agent".to_owned(),
        false => reached.join(" "),
    }
}

fn refreshed() -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return "this agent has no home".to_owned();
    };
    let login = std::path::Path::new(&home).join(LOGIN);

    match std::fs::read_to_string(&login) {
        Ok(found) => match std::fs::write(&login, format!("{found}{REFRESHED}")) {
            Ok(()) => format!("logged in as {found}"),
            Err(error) => format!("logged in as {found}, and could not refresh it: {error}"),
        },
        Err(_) => "no login was found".to_owned(),
    }
}

fn say(connection: &ConnectionTo<Client>, message: &str, said: &str) -> Result<()> {
    update(
        connection,
        SessionUpdate::AgentMessageChunk(chunk(Some(message), said)),
    )
}

fn update(connection: &ConnectionTo<Client>, update: SessionUpdate) -> Result<()> {
    connection.send_notification(SessionNotification::new(SESSION, update))
}

fn chunk(message: Option<&str>, said: &str) -> ContentChunk {
    ContentChunk::new(ContentBlock::Text(TextContent::new(said)))
        .message_id(message.map(MessageId::new))
}

fn models(current: impl Into<SessionConfigValueId>) -> SessionConfigOption {
    SessionConfigOption::new(
        MODEL_OPTION,
        "Model",
        SessionConfigKind::Select(SessionConfigSelect::new(
            current,
            vec![
                SessionConfigSelectOption::new(DEFAULT_MODEL, DEFAULT_MODEL),
                SessionConfigSelectOption::new(OTHER_MODEL, OTHER_MODEL),
            ],
        )),
    )
    .category(SessionConfigOptionCategory::Model)
}

/// Refuses to go on unless the client allows the call once, which is what makes a Run that
/// succeeded evidence that the round-trip completed.
async fn permission_to_use_a_tool(connection: &ConnectionTo<Client>) -> Result<()> {
    let outcome = connection
        .send_request(RequestPermissionRequest::new(
            SESSION,
            ToolCallUpdate::new(TOOL_CALL, ToolCallUpdateFields::new()),
            vec![allow_once(), reject_once()],
        ))
        .block_task()
        .await?
        .outcome;

    let RequestPermissionOutcome::Selected(selected) = outcome else {
        return Err(Error::internal_error()
            .data("the scripted agent was left without permission to proceed"));
    };
    if selected.option_id.0.as_ref() != ALLOW_ONCE {
        return Err(Error::internal_error().data(format!(
            "the scripted agent offered {ALLOW_ONCE} and was answered {}",
            selected.option_id.0
        )));
    }

    Ok(())
}

fn allow_once() -> PermissionOption {
    PermissionOption::new(ALLOW_ONCE, "Allow once", PermissionOptionKind::AllowOnce)
}

fn reject_once() -> PermissionOption {
    PermissionOption::new(
        "reject-once",
        "Reject once",
        PermissionOptionKind::RejectOnce,
    )
}
