//! A canned sequence over real stdio JSON-RPC, so the main suite drives kestrel's ACP client
//! over the wire rather than over a shim above it, with no network and no model spend.

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, ContentChunk, Cost, InitializeRequest, InitializeResponse,
    MessageId, NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionKind, Plan,
    PlanEntry, PlanEntryPriority, PlanEntryStatus, PromptCapabilities, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, SessionNotification,
    SessionUpdate, StopReason, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, UsageUpdate,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Error, Result, Stdio};
use clap::Parser;
use kestrel_scripted_agent::Script;

const SESSION: &str = "scripted";
const TOOL_CALL: &str = "call-1";
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

                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                        AgentCapabilities::new().prompt_capabilities(PromptCapabilities::new()),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_new: NewSessionRequest, responder, _connection| {
                responder.respond(NewSessionResponse::new(SESSION))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_prompt: PromptRequest, responder, connection| {
                // The turn asks the client a question of its own, so it cannot run inside the
                // dispatch loop that would have to carry the answer.
                connection.clone().spawn(async move {
                    let stop = play(script, &connection).await?;
                    responder.respond(PromptResponse::new(stop))
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new())
        .await
}

async fn play(script: Script, connection: &ConnectionTo<Client>) -> Result<StopReason> {
    if script == Script::Refuses {
        say(connection, "message-1", "this is not work I will do")?;
        return Ok(StopReason::Refusal);
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
