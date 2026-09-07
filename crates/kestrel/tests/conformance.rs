//! The suite kestrel's v1 gate is judged at (ADR-0007): kestrel's ACP client against two agents
//! of different lineages, one that speaks the protocol natively and one reached through an
//! adapter, both driving a real model.
//!
//! Every assertion here is asked of ACP rather than of a binary. What each agent is called, how
//! it is logged in and how it is configured live in `support::lineage`, which is the setup; a
//! failure in this file names the promise ACP makes that kestrel relied on and did not get.
//!
//! Both agents here advertise a model a client may select, so the runtime that advertises none
//! is the scripted ACP agent's to play, in `tests/acp.rs`; what this suite has instead is each
//! runtime naming the same model differently, and refusing the other's name for it.
//!
//! It costs network and model spend, so it is gated to nightly and to a release rather than run
//! per commit, and every test is ignored by default. What each Run spent is written to
//! `target/conformance-spend.md` for the job that ran it to publish.

mod support;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use kestrel::compute::{Docker, Driver, Environment};
use kestrel::domain::{Exit, Run, RunId, RunState, Session, Usage};
use kestrel::link::Instruction;
use kestrel::link::credential::Secret;
use support::Harness;
use support::diagnostics::Diagnostics;
use support::lineage::{DONE, Lineage};

/// A real agent starts slowly and a real model answers slowly, and a free one answers slowly
/// twice when it is busy.
const PATIENCE: Duration = Duration::from_secs(420);

/// What one Run of this suite may cost, in the currency the agent reports. The models it runs
/// on are free, so anything above nothing is a model that started charging.
const CEILING: f64 = 0.01;

/// How many turns an unanswered gateway is given before the suite gives up on judging one, and
/// how long it is left alone between them.
const ATTEMPTS: usize = 3;
const BREATHE: Duration = Duration::from_secs(30);

/// An ACP authentication method no agent advertises, because it is not one.
const UNOFFERED_LOGIN: &str = "a-login-no-agent-offers";

/// One conformance Run: the Environment executing it and what the supervisor in it says.
/// Provisioned through the `Compute` port rather than through the work role, because the
/// gateway the agent is pointed at is this suite's and has to reach the Environment before the
/// turn starts.
struct Driven {
    lineage: Lineage,
    run: Run,
    session: Session,
    environment: Environment,
    diagnostics: Diagnostics,
}

impl Driven {
    async fn started(harness: &Harness, lineage: Lineage, model: &str) -> Self {
        Self::logged_in_with(harness, lineage, model, lineage.auth()).await
    }

    async fn logged_in_with(harness: &Harness, lineage: Lineage, model: &str, auth: &str) -> Self {
        let session = a_session(harness, lineage, model).await;
        let (run, credential) = harness.dispatch_run(session.id).await;
        let (mut environment, mut diagnostics) =
            provisioned(harness, lineage, &session, run.id, &credential, auth);

        diagnostics.wait_until_it_says("reported connected").await;
        lineage.configure(&mut environment);
        harness.instruct(&run, Instruction::Start).await;

        Self {
            lineage,
            run,
            session: harness.show_session(session.id).await,
            environment,
            diagnostics,
        }
    }

    /// Killed once it is there to kill, because a real agent takes its time starting and the
    /// turn is what this has to land in the middle of.
    ///
    /// The image carries no `ps` and no `pkill`, so the process is found where the kernel keeps
    /// it. Matched from the front so the shell doing the matching is not itself a hit.
    async fn kill_the_agent_runtime(&mut self) {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        loop {
            let killed = self
                .environment
                .exec(&[
                    "sh",
                    "-c",
                    r#"for p in /proc/[0-9]*; do case "$(tr -d '\0' < "$p/cmdline" 2>/dev/null)" in "$1"*) kill -9 "${p#/proc/}" && echo "${p#/proc/}";; esac; done"#,
                    "sh",
                    self.lineage.process(),
                ])
                .expect("the agent runtime should be signalled")
                .finish()
                .expect("the signal should land");

            if !killed.out.is_empty() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no agent runtime ever started to be killed. {self}"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// The tool call the agent asked permission for, as the supervisor named it. What a Run did
    /// inside itself is the Run's business, so this is what no Transcript may carry.
    fn the_tool_call_it_asked_about(&self) -> String {
        let (_, named) = self
            .diagnostics
            .everything_it_said()
            .split_once("tool call ")
            .map(|(before, after)| (before.to_owned(), after.to_owned()))
            .expect("the supervisor should say which tool call it allowed");

        named
            .split_whitespace()
            .next()
            .expect("a tool call the supervisor named")
            .to_owned()
    }

    fn destroy(self) {
        self.environment
            .destroy()
            .expect("the environment should be destroyed");
    }
}

impl fmt::Display for Driven {
    /// Everything a failure here needs to be legible: which agent, and what its Environment
    /// said while the Run was in flight.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the {:?} agent {} said:\n{}",
            self.lineage,
            self.lineage.command(),
            self.diagnostics.everything_it_said()
        )
    }
}

fn provisioned(
    harness: &Harness,
    lineage: Lineage,
    session: &Session,
    run: RunId,
    credential: &Secret,
    auth: &str,
) -> (Environment, Diagnostics) {
    let link = harness.link_from_an_environment();
    let run_id = run.to_string();
    let mut variables = vec![
        ("KESTREL_LINK".to_owned(), link),
        ("KESTREL_RUN".to_owned(), run_id),
        (
            "KESTREL_RUN_CREDENTIAL".to_owned(),
            credential.as_str().to_owned(),
        ),
        (
            "KESTREL_AGENT_RUNTIME".to_owned(),
            lineage.command().to_owned(),
        ),
        ("KESTREL_AGENT_AUTH".to_owned(), auth.to_owned()),
        (
            "KESTREL_AGENT_MODEL".to_owned(),
            session.agent.model.clone().unwrap_or_default(),
        ),
    ];
    variables.extend(lineage.variables());

    let borrowed: Vec<(&str, &str)> = variables
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let mut environment = Driver::Docker(Docker::provisioning_from(lineage.image()))
        .provision(run, &borrowed)
        .expect("the environment should provision");
    let pipe = environment
        .take_stderr()
        .expect("the supervisor's diagnostics should be piped");

    (environment, Diagnostics::pumped("the environment", pipe))
}

async fn a_session(harness: &Harness, lineage: Lineage, model: &str) -> Session {
    let organization = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &organization,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    harness
        .declare_agent(&organization, "builder", lineage.command(), Some(model))
        .await;
    for (variable, secret) in lineage.credentials(&Lineage::key()) {
        harness
            .hold_provider_credential(&organization, &variable, &secret)
            .await;
    }

    harness.open_session("acme", "kestrel", "builder").await
}

async fn ended(harness: &Harness, driven: &Driven) -> Run {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let run = harness.run(driven.run.id).await;
        if run.state == RunState::Ended {
            return run;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run {} is {} and never ended. {driven}",
            run.id,
            run.state
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn transcript(harness: &Harness, session: &Session) -> Vec<String> {
    harness
        .transcript(session.id)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect()
}

/// What the Run spent, kept where the job that ran the suite can publish it. A gate whose cost
/// nobody can see is one nobody can keep bounded.
fn record(lineage: Lineage, model: &str, usage: Option<&Usage>) {
    let spent = match usage {
        Some(usage) => usage.to_string(),
        None => "nothing it reported".to_owned(),
    };
    let ledger =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/conformance-spend.md");

    let mut line = format!("- {lineage:?} (`{model}`): {spent}\n");
    if let Ok(existing) = std::fs::read_to_string(&ledger) {
        line = format!("{existing}{line}");
    }
    let _ = std::fs::write(&ledger, line);
}

/// A turn the gateway never answered says nothing about whether ACP was kept, so it is worked
/// again rather than judged: the models this suite runs on are free, and free is rate-limited
/// and best-effort both.
enum Judged {
    Kept,
    Unanswered,
}

async fn a_turn(lineage: Lineage) {
    for attempt in 1..=ATTEMPTS {
        if let Judged::Kept = a_turn_once(lineage).await {
            return;
        }
        if attempt < ATTEMPTS {
            tokio::time::sleep(BREATHE).await;
        }
    }

    panic!("the gateway answered none of {ATTEMPTS} turns, so none of them judged ACP");
}

/// One turn, and everything ACP promises about one: what the agent says reaches the Transcript
/// and what it does inside the Run does not, the permission round-trip is answered and the
/// agent goes on, the model the Run's Agent named is the one it was set to, what it used is
/// recorded, and `end_turn` is a Run that succeeded.
async fn a_turn_once(lineage: Lineage) -> Judged {
    let harness = Harness::boot_reachable_from_an_environment().await;
    let mut driven = Driven::started(&harness, lineage, &lineage.model()).await;

    let ended = ended(&harness, &driven).await;
    driven.diagnostics.drain();

    let said = transcript(&harness, &driven.session).await.join("\n");
    let because = match &ended.exit {
        Some(Exit::Failed { because }) => because.as_str(),
        _ => "",
    };
    if !said.to_lowercase().contains(DONE) && (unanswered(&said) || unanswered(because)) {
        driven.destroy();
        harness.teardown().await;
        return Judged::Unanswered;
    }

    assert_eq!(
        ended.exit,
        Some(Exit::Succeeded),
        "ACP: a turn that ends with `end_turn` is a Run that succeeded. {driven}"
    );
    assert!(
        driven
            .diagnostics
            .said(&format!("selected the model {}", lineage.model())),
        "ACP: `session/set_config_option` sets the model a client asked for. {driven}"
    );

    assert!(
        driven.diagnostics.said("allowed once"),
        "ACP: `session/request_permission` is a round-trip, and this one was never answered. \
         {driven}"
    );

    // Said only once the tool call it asked about has run, so a Transcript carrying it is an
    // agent that was answered and went on.
    assert!(
        said.to_lowercase().contains(DONE),
        "ACP: `agent_message_chunk` is what the agent said, and it reaches the Transcript. \
         the transcript is:\n{said}\n{driven}"
    );
    let call = driven.the_tool_call_it_asked_about();
    assert!(
        !said.contains(&call),
        "ACP: a `tool_call` is what happened inside the Run, and reaches no Transcript. \
         the transcript carries the tool call {call}:\n{said}"
    );

    let usage = ended.usage.as_ref();
    record(lineage, &lineage.model(), usage);
    let spent = usage
        .and_then(|usage| usage.cost.as_ref())
        .map_or(0.0, |cost| cost.amount);
    assert!(
        spent <= CEILING,
        "this run spent {spent}, and the suite runs on models that cost nothing"
    );

    driven.destroy();
    harness.teardown().await;

    Judged::Kept
}

/// What an agent says when the gateway turned it away or was not there at all, in the words two
/// lineages of agent both pass through from it.
fn unanswered(said: &str) -> bool {
    let said = said.to_lowercase();

    [
        "429",
        "rate limit",
        "too many requests",
        "cannot connect",
        "unable to connect",
    ]
    .iter()
    .any(|turned_away| said.contains(turned_away))
}

/// An agent that dies mid-turn: the Run ends with an exit status rather than hanging on a
/// connection ACP gives a client no way to reopen.
async fn an_agent_that_dies(lineage: Lineage) {
    let harness = Harness::boot_reachable_from_an_environment().await;
    let mut driven = Driven::started(&harness, lineage, &lineage.model()).await;

    driven
        .diagnostics
        .wait_until_it_says("reported started")
        .await;
    driven.kill_the_agent_runtime().await;

    let ended = ended(&harness, &driven).await;
    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and its agent was killed mid-turn. {driven}",
            ended.exit
        );
    };
    assert!(!because.is_empty(), "the run failed without saying why");

    record(lineage, &lineage.model(), ended.usage.as_ref());
    driven.destroy();
    harness.teardown().await;
}

/// A model config option is optional and a runtime advertises the values it will honour, so an
/// Agent naming one outside them is a Run that fails rather than one that quietly runs on the
/// runtime's default (ADR-0007).
async fn a_model_the_agent_does_not_offer(lineage: Lineage) {
    let harness = Harness::boot_reachable_from_an_environment().await;
    let driven = Driven::started(&harness, lineage, &lineage.unoffered_model()).await;

    let ended = ended(&harness, &driven).await;
    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and its agent named a model the runtime does not offer. {driven}",
            ended.exit
        );
    };
    assert!(
        because.contains(&lineage.unoffered_model()),
        "the run failed without naming the model it could not have: {because}"
    );

    driven.destroy();
    harness.teardown().await;
}

/// ACP has an agent advertise the methods it can be logged in with and offers a client no way
/// to choose between them, so which one kestrel uses is configuration. One the agent does not
/// advertise is a Run that fails rather than one left waiting at a login.
async fn a_login_the_agent_does_not_offer(lineage: Lineage) {
    let harness = Harness::boot_reachable_from_an_environment().await;
    let driven = Driven::logged_in_with(&harness, lineage, &lineage.model(), UNOFFERED_LOGIN).await;

    let ended = ended(&harness, &driven).await;
    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and kestrel was configured to log its agent in with a method \
             the agent does not offer. {driven}",
            ended.exit
        );
    };
    assert!(
        because.contains(UNOFFERED_LOGIN),
        "the run failed without naming the login it could not use: {because}"
    );

    driven.destroy();
    harness.teardown().await;
}

macro_rules! against_both_lineages {
    ($($assertion:ident),+ $(,)?) => {
        mod native {
            use super::*;

            $(
                #[tokio::test]
                #[ignore = "costs network and model spend"]
                async fn $assertion() {
                    super::$assertion(Lineage::Native).await;
                }
            )+
        }

        mod adapter {
            use super::*;

            $(
                #[tokio::test]
                #[ignore = "costs network and model spend"]
                async fn $assertion() {
                    super::$assertion(Lineage::Adapter).await;
                }
            )+
        }
    };
}

against_both_lineages!(
    a_turn,
    an_agent_that_dies,
    a_model_the_agent_does_not_offer,
    a_login_the_agent_does_not_offer,
);
