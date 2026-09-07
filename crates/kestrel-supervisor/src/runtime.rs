//! kestrel as an ACP client (ADR-0007): no contract of kestrel's, and no branch on which Agent
//! Runtime is on the other end of one.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr as _;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthMethod, AuthenticateRequest, ContentBlock, ContentChunk, ErrorCode, InitializeRequest,
    NewSessionRequest, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigId, SessionConfigKind,
    SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOptions,
    SessionConfigValueId, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    StopReason, TextContent,
};
use agent_client_protocol::{AcpAgent, Client, ConnectionTo, Error};

use crate::link::{Cost, Exit, Usage};
use crate::permission::{self, Subject};

/// Nothing on the link carries work for a Run, so every Run asks the same thing.
const PROMPT: &str = "Do the work this environment was provisioned for.";

/// What this Environment was configured to drive, and what the Run asks of it. Which Agent
/// Runtime is on the other end is the configuration's business, never this module's.
#[derive(Debug, Default)]
pub struct Runtime {
    pub command: String,
    /// The ACP authentication method to log the agent in with, for an agent that requires one.
    pub auth: Option<String>,
    /// The model the Run's Agent named, if it named one.
    pub model: Option<String>,
}

pub struct Worked {
    pub said: Vec<String>,
    pub usage: Option<Usage>,
    pub allowed: Vec<Subject>,
    pub on: Option<On>,
    pub exit: Exit,
}

/// Which model the agent works the turn on — the one its Agent named, or the one the runtime
/// defaults to when it named none — and every model the runtime offered to be set to.
pub struct On {
    pub model: String,
    pub offered: Vec<String>,
}

/// What to ask the agent to set, and what it is on once it has. Nothing is set for an Agent
/// that named no model: the agent is already on the default it advertised.
struct Selects {
    id: Option<SessionConfigId>,
    on: On,
}

/// Everything that can go wrong here is an exit status: a Run ends with one however it went.
///
/// `provider` reaches the agent's own process and nothing else: not this one's environment, not
/// a file, and not ACP, which carries no credentials (ADR-0007).
pub async fn work(
    runtime: &Runtime,
    provider: BTreeMap<String, String>,
    entries: &[crate::link::Entry],
) -> Worked {
    let heard = Arc::new(Mutex::new(Heard::default()));

    let spawn = match AcpAgent::from_str(&runtime.command) {
        Ok(spawn) => spawn,
        Err(error) => {
            return Heard::default().worked(failed(format!(
                "the agent runtime {:?} could not be spawned: {error}",
                runtime.command
            )));
        }
    };
    let spawn = AcpAgent::new(spawn.into_config().envs(provider));

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
        .connect_with(spawn, {
            // Owned rather than borrowed, because the connection outlives this call's frame.
            let (auth, model) = (runtime.auth.clone(), runtime.model.clone());
            let heard = Arc::clone(&heard);

            async move |connection: ConnectionTo<agent_client_protocol::Agent>| {
                a_turn(&connection, auth, model, entries, &heard).await
            }
        })
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

/// Everything kestrel asks of an agent, in the order ACP has a client ask it.
async fn a_turn(
    connection: &ConnectionTo<agent_client_protocol::Agent>,
    auth: Option<String>,
    model: Option<String>,
    entries: &[crate::link::Entry],
    heard: &Mutex<Heard>,
) -> Result<StopReason, Error> {
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
    if needs_a_human_at_a_terminal(&initialized.auth_methods) {
        return Err(Error::internal_error().data(
            "this agent authenticates only at an interactive terminal, and nobody is at one",
        ));
    }

    if let Some(method) = auth {
        if !initialized
            .auth_methods
            .iter()
            .any(|offered| offered.id().0.as_ref() == method)
        {
            return Err(Error::internal_error().data(format!(
                "kestrel is configured to log this agent in with {method:?}, and it offers {}",
                offered(&initialized.auth_methods)
            )));
        }
        connection
            .send_request(AuthenticateRequest::new(method))
            .block_task()
            .await?;
    }

    let set_up = connection
        .send_request(NewSessionRequest::new(working_directory()))
        .block_task()
        .await
        .map_err(|error| unlogged_in(error, &initialized.auth_methods))?;

    if let Some(selects) = selects_the_model(
        set_up.config_options.as_deref().unwrap_or_default(),
        model.as_deref(),
    )? {
        if let Some(id) = selects.id {
            connection
                .send_request(SetSessionConfigOptionRequest::new(
                    set_up.session_id.clone(),
                    id,
                    SessionConfigValueId::new(selects.on.model.clone()),
                ))
                .block_task()
                .await?;
        }
        heard
            .lock()
            .expect("what the agent said should not be poisoned")
            .on = Some(selects.on);
    }

    let answered = connection
        .send_request(PromptRequest::new(
            set_up.session_id,
            vec![ContentBlock::Text(TextContent::new(prompt(entries)))],
        ))
        .block_task()
        .await?;

    Ok(answered.stop_reason)
}

fn prompt(entries: &[crate::link::Entry]) -> String {
    if entries.is_empty() {
        return PROMPT.to_owned();
    }

    let context = entries
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    format!("Earlier context, oldest first:\n{context}\n\n{PROMPT}")
}

/// ACP's `terminal` method launches an interactive process for someone to log in at, so an
/// agent offering nothing else cannot be driven headlessly and is refused here rather than
/// prompted and left waiting (ADR-0007).
fn needs_a_human_at_a_terminal(offered: &[AuthMethod]) -> bool {
    !offered.is_empty()
        && offered
            .iter()
            .all(|method| matches!(method, AuthMethod::Terminal(_)))
}

/// ACP requires the agent to be logged in before `session/new`, and offers no way to tell which of
/// several methods a client with nobody at a keyboard should pick, so the method is
/// configuration and an agent that needs one kestrel was not given fails here.
fn unlogged_in(error: Error, offered_methods: &[AuthMethod]) -> Error {
    if error.code != ErrorCode::AuthRequired {
        return error;
    }

    Error::internal_error().data(format!(
        "this agent must be logged in before it answers session/new, and kestrel was configured \
         with no method to log it in with. it offers {}",
        offered(offered_methods)
    ))
}

fn offered(methods: &[AuthMethod]) -> String {
    if methods.is_empty() {
        return "none".to_owned();
    }

    methods
        .iter()
        .map(|method| method.id().0.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Config options are optional and every agent ships a default (ADR-0007), so an agent may
/// offer no model to select. One whose Agent named a model then fails rather than quietly
/// running on something else; one whose Agent named none runs on a model nobody can name.
fn selects_the_model(
    offered: &[SessionConfigOption],
    named: Option<&str>,
) -> Result<Option<Selects>, Error> {
    let selectable = offered
        .iter()
        .find(|option| option.category == Some(SessionConfigOptionCategory::Model))
        .and_then(|option| match &option.kind {
            SessionConfigKind::Select(select) => Some((&option.id, select)),
            _ => None,
        });

    let Some((id, select)) = selectable else {
        return match named {
            Some(model) => Err(Error::internal_error().data(format!(
                "this agent lets no client select a model, and this run's agent named {model}"
            ))),
            None => Ok(None),
        };
    };
    let offered: Vec<String> = selectable_values(&select.options)
        .map(|value| value.to_string())
        .collect();

    let Some(model) = named else {
        return Ok(Some(Selects {
            id: None,
            on: On {
                model: select.current_value.0.to_string(),
                offered,
            },
        }));
    };
    if !offered.iter().any(|value| value == model) {
        return Err(Error::internal_error().data(format!(
            "this agent does not offer the model {model}, which this run's agent named"
        )));
    }

    Ok(Some(Selects {
        id: Some(id.clone()),
        on: On {
            model: model.to_owned(),
            offered,
        },
    }))
}

fn selectable_values(options: &SessionConfigSelectOptions) -> impl Iterator<Item = Arc<str>> {
    let values: Vec<Arc<str>> = match options {
        SessionConfigSelectOptions::Ungrouped(options) => options
            .iter()
            .map(|option| option.value.0.clone())
            .collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter().map(|option| option.value.0.clone()))
            .collect(),
        _ => Vec::new(),
    };

    values.into_iter()
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
    on: Option<On>,
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
            on: self.on,
            exit,
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{
        AuthMethodAgent, AuthMethodTerminal, Plan, SessionConfigSelect, SessionConfigSelectGroup,
        SessionConfigSelectOption, ToolCall, UsageUpdate,
    };

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
    fn an_agent_offering_only_a_terminal_to_log_in_at_cannot_be_driven() {
        assert!(needs_a_human_at_a_terminal(&[AuthMethod::Terminal(
            AuthMethodTerminal::new("terminal", "Log in at a terminal")
        )]));
    }

    #[test]
    fn an_agent_offering_something_else_as_well_can_be() {
        assert!(!needs_a_human_at_a_terminal(&[
            AuthMethod::Terminal(AuthMethodTerminal::new("terminal", "Log in at a terminal")),
            AuthMethod::Agent(AuthMethodAgent::new("its-own", "Log in as the agent asks")),
        ]));
    }

    #[test]
    fn an_agent_that_offers_nothing_needs_nothing() {
        assert!(!needs_a_human_at_a_terminal(&[]));
    }

    fn models(offered: &[&'static str]) -> Vec<SessionConfigOption> {
        vec![
            SessionConfigOption::new(
                "reasoning",
                "Reasoning",
                SessionConfigKind::Select(SessionConfigSelect::new(
                    "low",
                    vec![SessionConfigSelectOption::new("low", "Low")],
                )),
            ),
            SessionConfigOption::new(
                "model",
                "Model",
                SessionConfigKind::Select(SessionConfigSelect::new(
                    offered[0],
                    offered
                        .iter()
                        .map(|model| SessionConfigSelectOption::new(*model, *model))
                        .collect::<Vec<_>>(),
                )),
            )
            .category(SessionConfigOptionCategory::Model),
        ]
    }

    #[test]
    fn the_model_a_run_named_is_set_through_the_option_the_agent_categorized_as_one() {
        let selects = selects_the_model(&models(&["fast", "thorough"]), Some("thorough"))
            .expect("the model should be selectable")
            .expect("an agent that offers a model");

        assert_eq!(selects.id.expect("a model to set").0.as_ref(), "model");
        assert_eq!(selects.on.model, "thorough");
        assert_eq!(selects.on.offered, ["fast", "thorough"]);
    }

    #[test]
    fn a_run_that_named_no_model_sets_nothing_and_is_on_what_the_agent_already_was() {
        let selects = selects_the_model(&models(&["fast", "thorough"]), None)
            .expect("naming no model should not fail")
            .expect("an agent that offers a model");

        assert!(selects.id.is_none());
        assert_eq!(selects.on.model, "fast");
    }

    #[test]
    fn a_model_an_agent_does_not_offer_is_refused_rather_than_swapped_for_one_it_does() {
        let refused = selects_the_model(&models(&["fast"]), Some("thorough"))
            .err()
            .expect("a model the agent does not offer");

        assert!(
            refused
                .data
                .is_some_and(|why| why.to_string().contains("thorough"))
        );
    }

    #[test]
    fn an_agent_that_lets_no_client_select_a_model_fails_a_run_that_named_one() {
        let refused = selects_the_model(&[], Some("thorough"))
            .err()
            .expect("an agent with no model to select");

        assert!(
            refused
                .data
                .is_some_and(|why| why.to_string().contains("thorough"))
        );
    }

    #[test]
    fn an_agent_that_lets_no_client_select_a_model_works_a_run_that_named_none() {
        assert!(
            selects_the_model(&[], None)
                .expect("naming no model should not fail")
                .is_none()
        );
    }

    #[test]
    fn a_grouped_selection_is_searched_the_same_as_a_flat_one() {
        let grouped = vec![
            SessionConfigOption::new(
                "model",
                "Model",
                SessionConfigKind::Select(SessionConfigSelect::new(
                    "fast",
                    vec![SessionConfigSelectGroup::new(
                        "theirs",
                        "Theirs",
                        vec![SessionConfigSelectOption::new("thorough", "Thorough")],
                    )],
                )),
            )
            .category(SessionConfigOptionCategory::Model),
        ];

        assert!(selects_the_model(&grouped, Some("thorough")).is_ok());
    }

    #[test]
    fn an_agent_that_must_be_logged_in_says_what_it_offers_to_be_logged_in_with() {
        let offered = [AuthMethod::Agent(AuthMethodAgent::new(
            "its-own",
            "Log in as the agent asks",
        ))];

        let refused = unlogged_in(Error::auth_required(), &offered);

        assert!(
            refused
                .data
                .is_some_and(|why| why.to_string().contains("its-own"))
        );
    }

    #[test]
    fn an_error_that_is_not_about_being_logged_in_is_carried_as_it_came() {
        let refused = unlogged_in(Error::invalid_params().data("no"), &[]);

        assert_eq!(refused.code, ErrorCode::InvalidParams);
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
