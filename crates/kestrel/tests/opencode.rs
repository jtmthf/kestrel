//! The default Agent Runtime, driven over ACP (ADR-0007): the assertions the scripted ACP
//! agent already carries, made against the real binary the `kestrel-env` image ships.
//!
//! opencode is named where the spawn command is built and where its own configuration file is
//! written, and nowhere else: everything asserted here is asked of ACP.
//!
//! Every test here builds and runs the image, which a `cargo test` has no business doing on
//! its own, so they are ignored by default and CI runs them with `--ignored`.

mod support;

use std::time::Duration;

use kestrel::compute::{Docker, Driver, Environment};
use kestrel::domain::{Exit, Run, RunId, RunState, Session};
use kestrel::link::Instruction;
use kestrel::link::credential::Secret;
use serde_json::json;
use support::Harness;
use support::diagnostics::Diagnostics;
use support::image;
use support::model::{MARK, Model};

/// A real Agent Runtime starts slowly, and the whole turn is two round-trips to a model that
/// answers instantly, so this is nearly all startup.
const PATIENCE: Duration = Duration::from_secs(180);

const RUNTIME: &str = "opencode acp";

/// A Run, the Environment executing it, and what the supervisor in it says. Provisioned through
/// the `Compute` port rather than through the work role, because the model the Agent Runtime is
/// pointed at is this test's and has to reach the Workspace before the turn starts.
struct Driven {
    run: Run,
    environment: Environment,
    diagnostics: Diagnostics,
}

impl Driven {
    async fn in_an_environment(harness: &Harness, model: &Model) -> Self {
        let session = a_session(harness).await;
        let (run, credential) = harness.dispatch_run(session.id).await;
        let (environment, diagnostics) = provisioned(harness, run.id, &credential);

        let mut driven = Self {
            run,
            environment,
            diagnostics,
        };
        driven
            .diagnostics
            .wait_until_it_says("reported connected")
            .await;
        driven
            .environment
            .write_file("opencode.json", configured_with(model).as_bytes())
            .expect("the agent runtime should be configured");
        harness.instruct(&driven.run, Instruction::Start).await;

        driven
    }

    /// The image carries no `ps` and no `pkill`, so the process is found where the kernel
    /// keeps it. Matched from the front so the shell doing the matching is not itself a hit.
    fn kill_the_agent_runtime(&mut self) {
        let killed = self
            .environment
            .exec(&[
                "sh",
                "-c",
                r#"for p in /proc/[0-9]*; do case "$(tr -d '\0' < "$p/cmdline" 2>/dev/null)" in "$1"*) kill -9 "${p#/proc/}" && echo "${p#/proc/}";; esac; done"#,
                "sh",
                "opencode",
            ])
            .expect("the agent runtime should be signalled")
            .finish()
            .expect("the signal should land");

        assert!(
            !killed.out.is_empty(),
            "no agent runtime was running to kill"
        );
    }

    fn destroy(self) {
        self.environment
            .destroy()
            .expect("the environment should be destroyed");
    }
}

fn provisioned(harness: &Harness, run: RunId, credential: &Secret) -> (Environment, Diagnostics) {
    let mut environment = Driver::Docker(Docker::provisioning_from(image::built()))
        .provision(
            run,
            &[
                ("KESTREL_LINK", &harness.link_from_an_environment()),
                ("KESTREL_RUN", &run.to_string()),
                ("KESTREL_RUN_CREDENTIAL", credential.as_str()),
                ("KESTREL_AGENT_RUNTIME", RUNTIME),
            ],
        )
        .expect("the environment should provision");
    let pipe = environment
        .take_stderr()
        .expect("the supervisor's diagnostics should be piped");

    (environment, Diagnostics::pumped("the environment", pipe))
}

/// Every tool call is asked permission for, because a Policy that allows an operation outright
/// is not what this exercises: the round-trip that carries the answer is.
fn configured_with(model: &Model) -> String {
    json!({
        "provider": {
            "kestrel-test": {
                "npm": "@ai-sdk/openai-compatible",
                "name": "kestrel-test",
                "options": { "baseURL": model.base_url_from_an_environment(), "apiKey": "unused" },
                "models": { "canned": { "name": "canned" } },
            },
        },
        "model": "kestrel-test/canned",
        "permission": { "bash": "ask" },
    })
    .to_string()
}

async fn a_session(harness: &Harness) -> Session {
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
        .declare_agent(&organization, "builder", "opencode", "claude-opus-5")
        .await;

    harness.open_session("acme", "kestrel", "builder").await
}

async fn ended(harness: &Harness, run: RunId) -> Run {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let run = harness.run(run).await;
        if run.state == RunState::Ended {
            return run;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run {} is {} and never ended",
            run.id,
            run.state
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The supervisor says how it answered a permission request only once the turn is over, so
/// what the model has been asked for is the only sight of a turn still in flight.
async fn working_at_a_turn(model: &Model) {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    while !model.is_working_at_the_rest_of_a_turn() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the agent runtime never came back for the rest of a turn"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
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

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn a_run_drives_the_agent_runtime_through_a_turn_and_ends_with_an_exit_status() {
    let harness = Harness::boot_reachable_from_an_environment().await;
    let model = Model::serving();
    let mut driven = Driven::in_an_environment(&harness, &model).await;

    let ended = ended(&harness, driven.run.id).await;

    driven.diagnostics.drain();
    assert_eq!(
        ended.exit,
        Some(Exit::Succeeded),
        "the environment said:\n{}",
        driven.diagnostics.everything_it_said()
    );
    assert!(
        !model.asked().is_empty(),
        "the run ended without the agent runtime having reached a model at all"
    );

    driven.destroy();
    harness.teardown().await;
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn what_the_agent_says_reaches_the_transcript_and_what_it_does_inside_the_run_does_not() {
    let harness = Harness::boot_reachable_from_an_environment().await;
    let model = Model::serving();
    let mut driven = Driven::in_an_environment(&harness, &model).await;
    let session = harness.show_session(driven.run.session).await;

    ended(&harness, driven.run.id).await;
    driven.diagnostics.drain();

    let transcript = transcript(&harness, &session).await;
    assert_eq!(
        transcript
            .iter()
            .filter(|entry| entry.starts_with("said"))
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "said  builder  half of one message, and the other half".to_owned(),
            "said  builder  a second message".to_owned(),
        ],
        "the environment said:\n{}",
        driven.diagnostics.everything_it_said()
    );

    let transcript = transcript.join("\n");
    for inside_the_run in ["call-1", "bash", MARK] {
        assert!(
            !transcript.contains(inside_the_run),
            "the transcript carries {inside_the_run}, which happened inside the run:\n{transcript}"
        );
    }

    driven.destroy();
    harness.teardown().await;
}

/// The model serves the rest of the turn only once its tool call has been answered, so a
/// transcript carrying the second message is a round-trip the Agent Runtime came back from.
#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn a_permission_request_is_answered_and_the_agent_runtime_proceeds() {
    let harness = Harness::boot_reachable_from_an_environment().await;
    let model = Model::serving();
    let mut driven = Driven::in_an_environment(&harness, &model).await;
    let session = harness.show_session(driven.run.session).await;

    driven
        .diagnostics
        .wait_until_it_says("allowed once  tool call call-1")
        .await;
    let ended = ended(&harness, driven.run.id).await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert!(
        transcript(&harness, &session)
            .await
            .iter()
            .any(|entry| entry.ends_with("a second message")),
        "the agent runtime was answered and never went on"
    );

    driven.destroy();
    harness.teardown().await;
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn an_agent_runtime_that_dies_mid_run_ends_the_run_with_an_exit_status() {
    let harness = Harness::boot_reachable_from_an_environment().await;
    let model = Model::dawdling();
    let mut driven = Driven::in_an_environment(&harness, &model).await;

    working_at_a_turn(&model).await;
    driven.kill_the_agent_runtime();

    let ended = ended(&harness, driven.run.id).await;
    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and its agent runtime was killed mid-turn",
            ended.exit
        );
    };
    assert!(!because.is_empty(), "the run failed without saying why");

    driven.destroy();
    harness.teardown().await;
}
