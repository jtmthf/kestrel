//! kestrel as an ACP client (ADR-0007): no contract of kestrel's, and no branch on which Agent
//! Runtime is on the other end of one.

use std::path::PathBuf;
use std::str::FromStr as _;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, InitializeRequest, NewSessionRequest, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, StopReason, TextContent,
};
use agent_client_protocol::{AcpAgent, Client, ConnectionTo, Error};

use crate::link::{Cost, Exit, Usage};
use crate::permission::{self, Subject};

/// Nothing on the link carries work for a Run, so every Run asks the same thing.
const PROMPT: &str = "Do the work this environment was provisioned for.";

pub struct Worked {
    pub said: Vec<String>,
    pub usage: Option<Usage>,
    pub allowed: Vec<Subject>,
    pub exit: Exit,
}

/// Everything that can go wrong here is an exit status: a Run ends with one however it went.
pub async fn work(command: &str) -> Worked {
    let heard = Arc::new(Mutex::new(Heard::default()));

    let spawn = match AcpAgent::from_str(command) {
        Ok(spawn) => spawn,
        Err(error) => {
            return Heard::default().worked(failed(format!(
                "the agent runtime {command:?} could not be spawned: {error}"
            )));
        }
    };

    let stopped = Client
        .builder()
        .name("kestrel")
        .on_receive_notification(
            {
                let heard = Arc::clone(&heard);
                async move |notification: SessionNotification, _connection| {
                    heard
                        .lock()
                        .expect("what the agent said should not be poisoned")
                        .update(notification.update);
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            {
                let heard = Arc::clone(&heard);
                async move |request: RequestPermissionRequest, responder, _connection| {
                    let outcome = match permission::allow_once(&request.options) {
                        Some(option) => {
                            heard
                                .lock()
                                .expect("what the agent said should not be poisoned")
                                .allowed
                                .push(Subject::from(&request));
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                option,
                            ))
                        }
                        None => RequestPermissionOutcome::Cancelled,
                    };

                    responder.respond(RequestPermissionResponse::new(outcome))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(
            spawn,
            async |connection: ConnectionTo<agent_client_protocol::Agent>| {
                let initialized = connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                if initialized.protocol_version != ProtocolVersion::V1 {
                    return Err(Error::internal_error().data(format!(
                        "kestrel speaks ACP v1, and this agent answered v{}",
                        initialized.protocol_version
                    )));
                }

                let set_up = connection
                    .send_request(NewSessionRequest::new(working_directory()))
                    .block_task()
                    .await?;

                let answered = connection
                    .send_request(PromptRequest::new(
                        set_up.session_id,
                        vec![ContentBlock::Text(TextContent::new(PROMPT))],
                    ))
                    .block_task()
                    .await?;

                Ok(answered.stop_reason)
            },
        )
        .await;

    let heard = std::mem::take(
        &mut *heard
            .lock()
            .expect("what the agent said should not be poisoned"),
    );

    match stopped {
        Ok(stop) => heard.worked(ended(stop)),
        Err(error) => heard.worked(failed(error.to_string())),
    }
}

fn working_directory() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

fn failed(because: String) -> Exit {
    Exit::Failed { because }
}

fn ended(stop: StopReason) -> Exit {
    match stop {
        StopReason::EndTurn => Exit::Succeeded,
        StopReason::MaxTokens => failed("the agent ran out of tokens".to_owned()),
        StopReason::MaxTurnRequests => failed("the agent ran out of requests".to_owned()),
        StopReason::Refusal => failed("the agent refused the work".to_owned()),
        StopReason::Cancelled => failed("the agent was cancelled".to_owned()),
        other => failed(format!(
            "the agent stopped for a reason kestrel does not know: {other:?}"
        )),
    }
}

/// An Agent's reasoning, its plan and its tool calls are the Run's business, and are dropped here.
#[derive(Default)]
struct Heard {
    open: Option<Message>,
    said: Vec<String>,
    usage: Option<Usage>,
    allowed: Vec<Subject>,
}

#[derive(Default)]
struct Message {
    id: Option<String>,
    said: String,
}

impl Heard {
    fn update(&mut self, update: SessionUpdate) {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => self.chunk(&chunk),
            SessionUpdate::UsageUpdate(usage) => {
                self.usage = Some(Usage {
                    context_used: usage.used,
                    context_size: usage.size,
                    cost: usage.cost.map(|cost| Cost {
                        amount: cost.amount,
                        currency: cost.currency,
                    }),
                });
            }
            _ => {}
        }
    }

    /// A change of `messageId` starts a new message; chunks that share one are one message.
    fn chunk(&mut self, chunk: &ContentChunk) {
        let ContentBlock::Text(text) = &chunk.content else {
            return;
        };
        let id = chunk.message_id.as_ref().map(|id| id.0.to_string());

        match &mut self.open {
            Some(open) if open.id == id => open.said.push_str(&text.text),
            _ => {
                self.close();
                self.open = Some(Message {
                    id,
                    said: text.text.clone(),
                });
            }
        }
    }

    fn close(&mut self) {
        if let Some(open) = self.open.take() {
            self.said.push(open.said);
        }
    }

    fn worked(mut self, exit: Exit) -> Worked {
        self.close();

        Worked {
            said: self.said,
            usage: self.usage,
            allowed: self.allowed,
            exit,
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{Plan, ToolCall, UsageUpdate};

    use super::*;

    fn chunk(message: Option<&str>, said: &str) -> ContentChunk {
        ContentChunk::new(ContentBlock::Text(TextContent::new(said)))
            .message_id(message.map(agent_client_protocol::schema::v1::MessageId::new))
    }

    fn heard(updates: Vec<SessionUpdate>) -> Worked {
        let mut heard = Heard::default();
        for update in updates {
            heard.update(update);
        }

        heard.worked(Exit::Succeeded)
    }

    #[test]
    fn chunks_that_share_a_message_are_one_thing_said() {
        let worked = heard(vec![
            SessionUpdate::AgentMessageChunk(chunk(Some("one"), "half, ")),
            SessionUpdate::AgentMessageChunk(chunk(Some("one"), "and half")),
        ]);

        assert_eq!(worked.said, vec!["half, and half".to_owned()]);
    }

    #[test]
    fn a_new_message_starts_a_new_thing_said() {
        let worked = heard(vec![
            SessionUpdate::AgentMessageChunk(chunk(Some("one"), "the first")),
            SessionUpdate::AgentMessageChunk(chunk(Some("two"), "the second")),
        ]);

        assert_eq!(
            worked.said,
            vec!["the first".to_owned(), "the second".to_owned()]
        );
    }

    #[test]
    fn a_plan_a_tool_call_and_a_thought_are_heard_and_never_said() {
        let worked = heard(vec![
            SessionUpdate::Plan(Plan::new(Vec::new())),
            SessionUpdate::AgentThoughtChunk(chunk(Some("one"), "thinking")),
            SessionUpdate::ToolCall(ToolCall::new("call-1", "read README.md")),
        ]);

        assert!(worked.said.is_empty());
    }

    #[test]
    fn what_the_agent_used_is_kept_and_never_said() {
        let worked = heard(vec![SessionUpdate::UsageUpdate(UsageUpdate::new(12, 100))]);

        assert!(worked.said.is_empty());
        assert_eq!(
            worked.usage,
            Some(Usage {
                context_used: 12,
                context_size: 100,
                cost: None,
            })
        );
    }

    #[test]
    fn ending_the_turn_is_the_only_stop_reason_a_run_succeeds_on() {
        assert_eq!(ended(StopReason::EndTurn), Exit::Succeeded);

        for stop in [
            StopReason::MaxTokens,
            StopReason::MaxTurnRequests,
            StopReason::Refusal,
            StopReason::Cancelled,
        ] {
            assert!(matches!(ended(stop), Exit::Failed { .. }), "{stop:?}");
        }
    }
}
