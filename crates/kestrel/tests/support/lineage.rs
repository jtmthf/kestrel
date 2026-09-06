//! The two agents the conformance suite is judged against, and everything about them that ACP
//! does not carry: what each is spawned as, how each is logged in, which model each names, and
//! what each needs written into its Environment before a turn.
//!
//! This is the suite's setup, and the only place an agent has a name.

use kestrel::compute::Environment;
use serde_json::json;

/// The work every conformance Run is provisioned for. An agent reads it as the instructions
/// its workspace came with, which is the only lever a Run has over what an agent does: nothing
/// on the link carries work at 0.1, so every Run's prompt is the same sentence.
const AGENTS_MD: &str = "\
# The work

Run the shell command `touch $HOME/kestrel-was-here` and nothing else. When it has run, say
exactly: the work is done.
";

/// What the agent says once the tool call it asked about has run: a Transcript carrying it is
/// an agent that was answered and went on.
pub const DONE: &str = "the work is done";

/// The model both agents are pointed at: free on the gateway, so a run's spend is bounded at
/// nothing, and served over the one wire API both lineages can speak.
const MODEL: &str = "muse-spark-1.3-contributor-free";
const GATEWAY: &str = "https://opencode.ai/zen/v1";
const KEY: &str = "OPENCODE_API_KEY";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lineage {
    /// An agent that speaks ACP itself.
    Native,
    /// An agent reached through an adapter that speaks ACP on its behalf.
    Adapter,
}

impl Lineage {
    /// The gateway key, from the environment the suite was started in. A conformance run that
    /// cannot reach a model is a run that proves nothing, so this is loud rather than skipped.
    pub fn key() -> String {
        std::env::var(KEY).unwrap_or_else(|_| {
            panic!("the conformance suite runs against a real model: set {KEY}")
        })
    }

    pub const fn command(self) -> &'static str {
        match self {
            Lineage::Native => "opencode acp",
            Lineage::Adapter => "codex-acp",
        }
    }

    /// The ACP method the agent is logged in with, for one that will not open a session until
    /// it has been.
    pub const fn auth(self) -> &'static str {
        match self {
            Lineage::Native => "",
            Lineage::Adapter => "api-key",
        }
    }

    /// As the runtime advertises it: the same model is named differently across the line.
    pub fn model(self) -> String {
        match self {
            Lineage::Native => format!("opencode/{MODEL}"),
            Lineage::Adapter => MODEL.to_owned(),
        }
    }

    /// The same model, named the way the other lineage names it. What one runtime advertises
    /// is not what the other does, and a client that assumes one shape is wrong on the other.
    pub fn unoffered_model(self) -> String {
        match self {
            Lineage::Native => MODEL.to_owned(),
            Lineage::Adapter => format!("opencode/{MODEL}"),
        }
    }

    /// The image the Environment is provisioned from. The shipped one carries the native agent
    /// and nothing else (ADR-0007), so the adapter is built into a copy of it.
    pub fn image(self) -> &'static str {
        match self {
            Lineage::Native => super::image::built(),
            Lineage::Adapter => super::image::with_the_adapter(),
        }
    }

    /// What the agent's own process is called, for the Run that has it killed mid-turn.
    pub const fn process(self) -> &'static str {
        match self {
            Lineage::Native => "opencode",
            Lineage::Adapter => "node",
        }
    }

    /// The Provider Credentials the Organization holds for this agent. They reach the agent's
    /// own process at the spawn, and never over ACP (ADR-0007) or into the Environment.
    pub fn credentials(self, key: &str) -> Vec<(String, String)> {
        let mut credentials = vec![(KEY.to_owned(), key.to_owned())];

        if self == Lineage::Adapter {
            credentials.push(("CODEX_API_KEY".to_owned(), key.to_owned()));
        }

        credentials
    }

    /// Where the gateway is and how the agent behaves at one: configuration rather than
    /// credentials, so the Environment is provisioned with it.
    pub fn variables(self) -> Vec<(String, String)> {
        match self {
            Lineage::Native => Vec::new(),
            Lineage::Adapter => vec![
                ("MODEL_PROVIDER".to_owned(), "zen".to_owned()),
                ("NO_BROWSER".to_owned(), "1".to_owned()),
                ("INITIAL_AGENT_MODE".to_owned(), "read-only".to_owned()),
                ("CODEX_CONFIG".to_owned(), codex_config()),
            ],
        }
    }

    /// Written into the Environment before the Run is told to start. The Agent's model is not
    /// among it: that reaches the runtime over ACP, which is the point of setting it.
    pub fn configure(self, environment: &mut Environment) {
        environment
            .write_file("AGENTS.md", AGENTS_MD.as_bytes())
            .expect("the work should reach the workspace");

        if self == Lineage::Native {
            environment
                .write_file("opencode.json", opencode_config().as_bytes())
                .expect("the agent runtime should be configured");
        }
    }
}

/// Every tool call is asked permission for, because a policy that allows an operation outright
/// is not what the suite exercises: the round-trip that carries the answer is. Delegation is
/// off because a subagent's turn is one this suite neither bounds nor observes.
fn opencode_config() -> String {
    json!({
        "$schema": "https://opencode.ai/config.json",
        "permission": { "bash": "ask" },
        "tools": { "task": false },
    })
    .to_string()
}

/// The adapter takes the model provider as configuration rather than over ACP, because ACP
/// carries no credentials and the provider is where the key is spent.
fn codex_config() -> String {
    json!({
        "model": MODEL,
        "model_provider": "zen",
        "model_providers": {
            "zen": {
                "name": "OpenCode Zen",
                "base_url": GATEWAY,
                "env_key": KEY,
                "wire_api": "responses",
            },
        },
    })
    .to_string()
}
