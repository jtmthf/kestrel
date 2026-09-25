//! kestrel as an ACP client (ADR-0007): no contract of kestrel's, and no branch on which Agent
//! Runtime is on the other end of one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, AuthMethod, AuthenticateRequest, ContentBlock, ContentChunk, ErrorCode,
    InitializeRequest, InitializeResponse, LoadSessionRequest, NewSessionRequest, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigId, SessionConfigKind,
    SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOptions,
    SessionConfigValueId, SessionId, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, StopReason, TextContent,
};
use agent_client_protocol::{
    AcpAgent, Client, ConnectionTo, Error, LineDirection, is_incoming_transport_closed,
};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::link::{Cost, Usage};
use crate::permission::{self, Subject};

/// Nothing on the link carries work for a Run, so every Run asks the same thing.
const PROMPT: &str = "Do the work this environment was provisioned for.";

/// What this Environment was configured to drive, what the Run asks of it, and where what the
/// agent writes to stderr goes. Which Agent Runtime is on the other end is the configuration's
/// business, never this module's.
#[derive(Debug, Clone)]
pub struct Runtime {
    pub command: String,
    /// The ACP authentication method to log the agent in with, for an agent that requires one.
    pub auth: Option<String>,
    /// The model the Run named, if it named one.
    pub model: Option<String>,
    pub stderr: mpsc::UnboundedSender<String>,
}

/// Long enough for any log line worth reading, and short enough that a runaway one costs the
/// link little.
const LINE_LIMIT: usize = 4 * 1024;

/// One turn's account. `failed` says why the conversation is over; without it, the agent
/// answered and waits for another prompt.
pub struct Worked {
    pub said: Vec<String>,
    pub usage: Option<Usage>,
    pub allowed: Vec<Subject>,
    pub on: Option<On>,
    pub failed: Option<String>,
}

/// Which model the agent works the turn on — the one its Run named, or the one the runtime
/// defaults to when it named none — and every model the runtime offered to be set to.
pub struct On {
    pub model: String,
    pub offered: Vec<String>,
}

/// What to ask the agent to set, and what it is on once it has. Nothing is set for a Run
/// that named no model: the agent is already on the default it advertised.
struct Selects {
    id: Option<SessionConfigId>,
    on: On,
}

/// Long enough for an agent between turns to see its connection close; one mid-turn is cut off.
const CLOSING: Duration = Duration::from_millis(500);

/// One ACP conversation for the whole Run, held apart from the link so that losing the link loses
/// nothing of it (ADR-0024). Dropping it kills the agent.
pub struct Conversation {
    prompts: mpsc::UnboundedSender<String>,
    turns: mpsc::UnboundedReceiver<Worked>,
    task: JoinHandle<()>,
}

impl Conversation {
    /// `provider` reaches the agent's own process and nothing else: not this one's
    /// environment, not a file, and not ACP, which carries no credentials (ADR-0007).
    pub fn open(
        runtime: &Runtime,
        provider: BTreeMap<String, String>,
        first: String,
        root: PathBuf,
    ) -> Self {
        let (prompts, prompted) = mpsc::unbounded_channel();
        let (answered, turns) = mpsc::unbounded_channel();
        prompts
            .send(first)
            .expect("the conversation has not started, so nothing has hung up on it");
        let task = tokio::spawn(conversing(
            runtime.clone(),
            provider,
            root,
            prompted,
            answered,
        ));

        Self {
            prompts,
            turns,
            task,
        }
    }

    pub fn prompt(&self, prompt: String) {
        // A conversation that is over says so as its last turn, which is where that is heard.
        let _ = self.prompts.send(prompt);
    }

    /// Cancel-safe, so a caller may stop waiting on it and come back.
    pub async fn turn(&mut self) -> Worked {
        self.turns.recv().await.unwrap_or_else(|| {
            Heard::default().worked(Some("the agent conversation ended unannounced".to_owned()))
        })
    }

    /// Waited out, because the agent runs in a process group of its own that only its
    /// connection closing kills, and an exit that beats that leaves the agent behind.
    pub async fn end(mut self) {
        let (hung_up, _) = mpsc::unbounded_channel();
        drop(std::mem::replace(&mut self.prompts, hung_up));
        if tokio::time::timeout(CLOSING, &mut self.task).await.is_err() {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
    }
}

impl Drop for Conversation {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recovery {
    Resume,
    Load,
}

impl Recovery {
    /// Resume is preferred because it goes on without replaying the conversation back.
    fn offered(capabilities: &AgentCapabilities) -> Option<Self> {
        if capabilities.session_capabilities.resume.is_some() {
            return Some(Self::Resume);
        }

        capabilities.load_session.then_some(Self::Load)
    }
}

#[derive(Default)]
struct Continuity {
    conversed: Option<SessionId>,
    recovery: Option<Recovery>,
    in_flight: Option<String>,
    /// Cleared by an answered turn, so an agent that dies as soon as it is brought back is not
    /// brought back forever.
    recovered: bool,
}

impl Continuity {
    fn opened(&mut self, conversed: SessionId, recovery: Option<Recovery>) {
        self.conversed = Some(conversed);
        self.recovery = recovery;
    }

    fn answered(&mut self) {
        self.in_flight = None;
        self.recovered = false;
    }

    /// Never `session/new` again: a conversation standing in for the lost one would be passed
    /// off as its continuation (ADR-0024).
    fn recovering(&mut self, lost: String) -> Result<(), String> {
        if self.conversed.is_none() {
            return Err(lost);
        }
        if self.recovery.is_none() {
            return Err(format!(
                "the agent's process was lost ({lost}), and its runtime cannot resume the \
                 conversation it held"
            ));
        }
        if self.recovered {
            return Err(format!(
                "the agent's process was lost again ({lost}), with no turn answered since it \
                 was brought back"
            ));
        }
        self.recovered = true;

        Ok(())
    }
}

enum Ended {
    HungUp,
    Over(String),
    Lost(String),
}

/// Everything that can go wrong here ends the conversation, and is its last turn.
async fn conversing(
    runtime: Runtime,
    provider: BTreeMap<String, String>,
    root: PathBuf,
    mut prompts: mpsc::UnboundedReceiver<String>,
    turns: mpsc::UnboundedSender<Worked>,
) {
    let heard = Arc::new(Mutex::new(Heard::default()));
    let mut continuity = Continuity::default();

    let because = loop {
        let lost = match living(
            &runtime,
            &provider,
            &root,
            &mut prompts,
            &turns,
            &heard,
            &mut continuity,
        )
        .await
        {
            Ok(Ended::HungUp) => return,
            Ok(Ended::Over(because)) => break because,
            Ok(Ended::Lost(because)) => because,
            Err(error) => described(&error),
        };
        match continuity.recovering(lost.clone()) {
            Ok(()) => {
                let _ = runtime.stderr.send(format!(
                    "kestrel: the agent's process was lost ({lost}); bringing it back into its \
                     conversation"
                ));
            }
            Err(because) => break because,
        }
    };
    let _ = turns.send(taken(&heard).worked(Some(because)));
}

/// An `Err` is the connection itself failing, which is a loss.
async fn living(
    runtime: &Runtime,
    provider: &BTreeMap<String, String>,
    root: &Path,
    prompts: &mut mpsc::UnboundedReceiver<String>,
    turns: &mpsc::UnboundedSender<Worked>,
    heard: &Arc<Mutex<Heard>>,
    continuity: &mut Continuity,
) -> Result<Ended, Error> {
    let spawn = match AcpAgent::from_str(&runtime.command) {
        Ok(spawn) => spawn,
        Err(error) => {
            return Ok(Ended::Over(format!(
                "the agent runtime {:?} could not be spawned: {error}",
                runtime.command
            )));
        }
    };
    let stderr = runtime.stderr.clone();
    let spawn = AcpAgent::new(spawn.into_config().envs(provider.clone())).with_debug(
        move |line, direction| {
            if direction == LineDirection::Stderr {
                let _ = stderr.send(bounded(line));
            }
        },
    );

    Client
        .builder()
        .name("kestrel")
        .on_receive_notification(
            {
                let heard = Arc::clone(heard);
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
                let heard = Arc::clone(heard);
                async move |request: RequestPermissionRequest, responder, _connection| {
                    let mut heard = heard
                        .lock()
                        .expect("what the agent said should not be poisoned");
                    heard.produced = true;
                    let outcome = match permission::allow_once(&request.options) {
                        Some(option) => {
                            heard.allowed.push(Subject::from(&request));
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                option,
                            ))
                        }
                        None => RequestPermissionOutcome::Cancelled,
                    };
                    drop(heard);

                    responder.respond(RequestPermissionResponse::new(outcome))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(
            spawn,
            async |connection: ConnectionTo<agent_client_protocol::Agent>| {
                let conversed = match (continuity.conversed.clone(), continuity.recovery) {
                    (Some(conversed), Some(recovery)) => {
                        if let Err(error) =
                            recover(&connection, runtime, root, heard, &conversed, recovery).await
                        {
                            return Ok(ended(&error));
                        }
                        conversed
                    }
                    _ => match set_up(&connection, runtime, root, heard).await {
                        Ok((conversed, recovery)) => {
                            continuity.opened(conversed.clone(), recovery);
                            conversed
                        }
                        Err(error) => return Ok(ended(&error)),
                    },
                };

                loop {
                    let prompt = match continuity.in_flight.clone() {
                        Some(prompt) => prompt,
                        None => tokio::select! {
                            prompt = prompts.recv() => match prompt {
                                Some(prompt) => prompt,
                                None => return Ok(Ended::HungUp),
                            },
                            () = connection.incoming_closed() => {
                                return Ok(Ended::Lost(
                                    "it closed its connection between turns".to_owned(),
                                ));
                            }
                        },
                    };
                    continuity.in_flight = Some(prompt.clone());
                    let answered = match connection
                        .send_request(PromptRequest::new(
                            conversed.clone(),
                            vec![ContentBlock::Text(TextContent::new(prompt))],
                        ))
                        .block_task()
                        .await
                    {
                        Ok(answered) => answered,
                        Err(error) => return Ok(ended(&error)),
                    };
                    continuity.answered();

                    if let Some(because) = stopped_short(answered.stop_reason) {
                        return Ok(Ended::Over(because));
                    }
                    let this_turn = taken(heard);
                    let failed = this_turn
                        .produced_nothing()
                        .then(|| "the agent answered the prompt with nothing".to_owned());
                    if turns.send(this_turn.worked(failed)).is_err() {
                        return Ok(Ended::HungUp);
                    }
                }
            },
        )
        .await
}

fn ended(error: &Error) -> Ended {
    match is_incoming_transport_closed(error) {
        true => Ended::Lost(described(error)),
        false => Ended::Over(error.to_string()),
    }
}

/// What the agent's end of a lost connection said, without the library's own bookkeeping
/// around it.
fn described(error: &Error) -> String {
    let data = error.data.as_ref();
    data.and_then(|data| data.get("data"))
        .or(data)
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| error.to_string(), str::to_owned)
}

fn taken(heard: &Mutex<Heard>) -> Heard {
    std::mem::take(
        &mut *heard
            .lock()
            .expect("what the agent said should not be poisoned"),
    )
}

async fn initialized(
    connection: &ConnectionTo<agent_client_protocol::Agent>,
    auth: Option<&str>,
) -> Result<InitializeResponse, Error> {
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
            .send_request(AuthenticateRequest::new(method.to_owned()))
            .block_task()
            .await?;
    }

    Ok(initialized)
}

/// Everything kestrel asks of an agent before its first prompt, in the order ACP has a client
/// ask it.
async fn set_up(
    connection: &ConnectionTo<agent_client_protocol::Agent>,
    runtime: &Runtime,
    root: &Path,
    heard: &Mutex<Heard>,
) -> Result<(SessionId, Option<Recovery>), Error> {
    let initialized = initialized(connection, runtime.auth.as_deref()).await?;

    let set_up = connection
        .send_request(NewSessionRequest::new(root))
        .block_task()
        .await
        .map_err(|error| unlogged_in(error, &initialized.auth_methods))?;

    let on = select(
        connection,
        &set_up.session_id,
        set_up.config_options.as_deref(),
        runtime.model.as_deref(),
    )
    .await?;
    heard
        .lock()
        .expect("what the agent said should not be poisoned")
        .on = on;

    Ok((
        set_up.session_id,
        Recovery::offered(&initialized.agent_capabilities),
    ))
}

/// What the lost process said mid-turn is dropped, because that turn is prompted again, and so
/// is whatever a loaded conversation replays; what it used and was allowed still happened.
async fn recover(
    connection: &ConnectionTo<agent_client_protocol::Agent>,
    runtime: &Runtime,
    root: &Path,
    heard: &Mutex<Heard>,
    conversed: &SessionId,
    recovery: Recovery,
) -> Result<(), Error> {
    initialized(connection, runtime.auth.as_deref()).await?;
    let lost = taken(heard);

    let config_options = match recovery {
        Recovery::Resume => {
            connection
                .send_request(ResumeSessionRequest::new(conversed.clone(), root))
                .block_task()
                .await?
                .config_options
        }
        Recovery::Load => {
            connection
                .send_request(LoadSessionRequest::new(conversed.clone(), root))
                .block_task()
                .await?
                .config_options
        }
    };
    {
        let mut heard = heard
            .lock()
            .expect("what the agent said should not be poisoned");
        *heard = Heard {
            on: lost.on,
            usage: lost.usage,
            allowed: lost.allowed,
            ..Heard::default()
        };
    }

    select(
        connection,
        conversed,
        config_options.as_deref(),
        runtime.model.as_deref(),
    )
    .await?;

    Ok(())
}

async fn select(
    connection: &ConnectionTo<agent_client_protocol::Agent>,
    conversed: &SessionId,
    offered: Option<&[SessionConfigOption]>,
    model: Option<&str>,
) -> Result<Option<On>, Error> {
    let Some(selects) = selects_the_model(offered.unwrap_or_default(), model)? else {
        return Ok(None);
    };
    if let Some(id) = selects.id {
        connection
            .send_request(SetSessionConfigOptionRequest::new(
                conversed.clone(),
                id,
                SessionConfigValueId::new(selects.on.model.clone()),
            ))
            .block_task()
            .await?;
    }

    Ok(Some(selects.on))
}

pub fn prompt(entries: &[crate::link::Entry]) -> String {
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
/// offer no model to select. One whose Run named a model then fails rather than quietly
/// running on something else; one whose Run named none runs on a model nobody can name.
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
                "this agent lets no client select a model, and this run named {model}"
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
            "this agent does not offer the model {model}, which this run named"
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

fn bounded(line: &str) -> String {
    if line.len() <= LINE_LIMIT {
        return line.to_owned();
    }

    let mut end = LINE_LIMIT;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &line[..end])
}

/// Why a turn that stopped for anything but ending it ends the conversation too.
fn stopped_short(stop: StopReason) -> Option<String> {
    let because = match stop {
        StopReason::EndTurn => return None,
        StopReason::MaxTokens => "the agent ran out of tokens".to_owned(),
        StopReason::MaxTurnRequests => "the agent ran out of requests".to_owned(),
        StopReason::Refusal => "the agent refused the work".to_owned(),
        StopReason::Cancelled => "the agent was cancelled".to_owned(),
        other => format!("the agent stopped for a reason kestrel does not know: {other:?}"),
    };

    Some(because)
}

/// An Agent's reasoning, its plan and its tool calls are the Run's business, and are dropped here.
#[derive(Default)]
struct Heard {
    open: Option<Message>,
    said: Vec<String>,
    usage: Option<Usage>,
    allowed: Vec<Subject>,
    on: Option<On>,
    /// Whether this turn produced anything at all; a usage, command, mode or config update does
    /// not.
    produced: bool,
}

#[derive(Default)]
struct Message {
    id: Option<String>,
    said: String,
}

impl Heard {
    fn update(&mut self, update: SessionUpdate) {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                self.produced = true;
                self.chunk(&chunk);
            }
            SessionUpdate::AgentThoughtChunk(_)
            | SessionUpdate::Plan(_)
            | SessionUpdate::ToolCall(_)
            | SessionUpdate::ToolCallUpdate(_) => self.produced = true,
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

    fn produced_nothing(&self) -> bool {
        !self.produced
    }

    fn worked(mut self, failed: Option<String>) -> Worked {
        self.close();

        Worked {
            said: self.said,
            usage: self.usage,
            allowed: self.allowed,
            on: self.on,
            failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{
        AuthMethodAgent, AuthMethodTerminal, AvailableCommandsUpdate, ConfigOptionUpdate,
        CurrentModeUpdate, Plan, SessionCapabilities, SessionConfigSelect,
        SessionConfigSelectGroup, SessionConfigSelectOption, SessionModeId,
        SessionResumeCapabilities, ToolCall, ToolCallUpdate, ToolCallUpdateFields, UsageUpdate,
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

        heard.worked(None)
    }

    fn produced_something(updates: Vec<SessionUpdate>) -> bool {
        let mut heard = Heard::default();
        for update in updates {
            heard.update(update);
        }

        !heard.produced_nothing()
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
    fn a_turn_that_produced_nothing_produced_nothing() {
        assert!(!produced_something(Vec::new()));
    }

    #[test]
    fn bookkeeping_updates_alone_are_not_something_produced() {
        assert!(!produced_something(vec![
            SessionUpdate::UsageUpdate(UsageUpdate::new(12, 100)),
            SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(Vec::new())),
            SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(SessionModeId::new("build"))),
            SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(Vec::new())),
        ]));
    }

    #[test]
    fn a_message_a_thought_a_plan_and_a_tool_call_are_each_something_produced() {
        assert!(produced_something(vec![SessionUpdate::AgentMessageChunk(
            chunk(Some("one"), "said")
        )]));
        assert!(produced_something(vec![SessionUpdate::AgentThoughtChunk(
            chunk(Some("one"), "thinking")
        )]));
        assert!(produced_something(vec![SessionUpdate::Plan(Plan::new(
            Vec::new()
        ))]));
        assert!(produced_something(vec![SessionUpdate::ToolCall(
            ToolCall::new("call-1", "read README.md")
        )]));
        assert!(produced_something(vec![SessionUpdate::ToolCallUpdate(
            ToolCallUpdate::new("call-1", ToolCallUpdateFields::new())
        )]));
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
    fn a_line_within_the_limit_is_carried_whole() {
        assert_eq!(
            bounded("level=INFO message=init"),
            "level=INFO message=init"
        );
    }

    #[test]
    fn an_overlong_line_is_cut_short_and_says_so() {
        let overlong = "é".repeat(LINE_LIMIT);

        let carried = bounded(&overlong);

        assert!(carried.starts_with(&"é".repeat(LINE_LIMIT / 2)));
        assert!(carried.ends_with("… [truncated]"));
        assert!(carried.len() < LINE_LIMIT + 64);
    }

    #[test]
    fn ending_the_turn_is_the_only_stop_reason_the_conversation_goes_on_after() {
        assert_eq!(stopped_short(StopReason::EndTurn), None);

        for stop in [
            StopReason::MaxTokens,
            StopReason::MaxTurnRequests,
            StopReason::Refusal,
            StopReason::Cancelled,
        ] {
            assert!(stopped_short(stop).is_some(), "{stop:?}");
        }
    }

    #[test]
    fn an_agent_that_can_both_resume_and_load_a_session_is_resumed() {
        let capabilities = AgentCapabilities::new()
            .load_session(true)
            .session_capabilities(
                SessionCapabilities::new().resume(SessionResumeCapabilities::new()),
            );

        assert_eq!(Recovery::offered(&capabilities), Some(Recovery::Resume));
    }

    #[test]
    fn an_agent_that_can_only_load_a_session_is_loaded() {
        let capabilities = AgentCapabilities::new().load_session(true);

        assert_eq!(Recovery::offered(&capabilities), Some(Recovery::Load));
    }

    #[test]
    fn an_agent_that_can_do_neither_cannot_be_recovered() {
        assert_eq!(Recovery::offered(&AgentCapabilities::new()), None);
    }

    fn conversing_with(recovery: Option<Recovery>) -> Continuity {
        Continuity {
            conversed: Some(SessionId::new("a-conversation")),
            recovery,
            ..Continuity::default()
        }
    }

    #[test]
    fn a_process_lost_before_it_opened_a_session_has_nothing_to_recover() {
        let mut continuity = Continuity {
            recovery: Some(Recovery::Load),
            ..Continuity::default()
        };

        assert_eq!(
            continuity.recovering("it exited".to_owned()),
            Err("it exited".to_owned())
        );
    }

    #[test]
    fn a_runtime_that_cannot_resume_a_session_ends_the_conversation_saying_so() {
        let refused = conversing_with(None)
            .recovering("it exited".to_owned())
            .expect_err("nothing to recover with");

        assert!(
            refused.contains("it exited") && refused.contains("cannot resume"),
            "{refused}"
        );
    }

    #[test]
    fn an_agent_lost_again_before_it_answers_is_not_brought_back_twice() {
        let mut continuity = conversing_with(Some(Recovery::Load));
        assert_eq!(continuity.recovering("it exited".to_owned()), Ok(()));

        let refused = continuity
            .recovering("it exited".to_owned())
            .expect_err("a second loss without an answer between");

        assert!(refused.contains("again"), "{refused}");
    }
}
