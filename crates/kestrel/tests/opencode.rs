//! The default Harness, driven over ACP (ADR-0007): the assertions the scripted ACP
//! agent already carries, made against the real binary the `kestrel-env` image ships.
//!
//! opencode is named where the spawn command is built and where its own configuration file is
//! written, and nowhere else: everything asserted here is asked of ACP.
//!
//! Every test here builds and runs the image, which a `cargo test` has no business doing on
//! its own, so they are ignored by default and CI runs them with `--ignored`.

mod support;

use std::time::Duration;

use kestrel::compute::{Docker, Driver, Instance, Supervisor};
use kestrel::domain::{Exit, Run, RunId, Session};
use kestrel::link::credential::Secret;
use serde_json::json;
use support::Kestrel;
use support::diagnostics::Diagnostics;
use support::image;
use support::model::{MARK, Model};

/// A real Harness starts slowly, and the whole turn is two round-trips to a model that
/// answers instantly, so this is nearly all startup.
const PATIENCE: Duration = Duration::from_secs(180);

const HARNESS: &str = "opencode acp";
/// The model the stub endpoint serves, named as the Harness advertises it: the provider
/// this Instance is configured with, and the one model in it.
const MODEL: &str = "kestrel-test/canned";
/// opencode fixes its model catalog at the first model it sees, and its built-in models are ready
/// before a configured provider's, so an empty snapshot leaves only the model this Run named.
const MODEL_SNAPSHOT: &str = "/workspace/models.json";

/// A Run, the Instance executing it, and what the supervisor on it says. Provisioned through
/// the `Compute` port rather than through the work role, because the model the Harness is
/// pointed at is this test's and has to reach the Workspace before the turn starts.
struct Driven {
    run: Run,
    instance: Instance,
    supervisor: Supervisor,
    diagnostics: Diagnostics,
}

impl Driven {
    async fn in_an_environment(kestrel: &Kestrel, model: &Model) -> Self {
        let session = a_session(kestrel).await;
        let (run, credential) = kestrel.dispatch_run(session.id).await;
        let (instance, supervisor, diagnostics) = provisioned(
            kestrel,
            run.id,
            &credential,
            session.agent.model.as_deref().unwrap_or_default(),
        );

        let mut driven = Self {
            run,
            instance,
            supervisor,
            diagnostics,
        };
        driven
            .diagnostics
            .wait_until_it_says("reported connected")
            .await;
        driven
            .instance
            .write_file("opencode.json", configured_with(model).as_bytes())
            .expect("the harness should be configured");
        driven
            .instance
            .write_file("models.json", b"{}")
            .expect("the harness's model snapshot should be written");
        kestrel.start(&driven.run).await;

        driven
    }

    /// The image carries no `ps` and no `pkill`, so the process is found where the kernel
    /// keeps it. Matched from the front so the shell doing the matching is not itself a hit.
    fn kill_the_harness(&mut self) {
        let killed = self
            .instance
            .exec(&[
                "sh",
                "-c",
                r#"for p in /proc/[0-9]*; do case "$(tr -d '\0' < "$p/cmdline" 2>/dev/null)" in "$1"*) kill -9 "${p#/proc/}" && echo "${p#/proc/}";; esac; done"#,
                "sh",
                "opencode",
            ])
            .expect("the harness should be signalled")
            .finish()
            .expect("the signal should land");

        assert!(!killed.out.is_empty(), "no harness was running to kill");
    }

    fn destroy(self) {
        self.supervisor
            .stop()
            .expect("the supervisor should be stopped");
        self.instance
            .destroy()
            .expect("the instance should be destroyed");
    }
}

fn provisioned(
    kestrel: &Kestrel,
    run: RunId,
    credential: &Secret,
    model: &str,
) -> (Instance, Supervisor, Diagnostics) {
    let mut instance = Driver::Docker(Docker::provisioning_from(image::built()))
        .provision(run)
        .expect("the instance should provision");
    let mut supervisor = instance
        .supervise(&[
            ("KESTREL_LINK", &kestrel.link_from_an_environment()),
            ("KESTREL_RUN", &run.to_string()),
            ("KESTREL_RUN_CREDENTIAL", credential.as_str()),
            ("KESTREL_HARNESS_COMMAND", HARNESS),
            ("KESTREL_AGENT_MODEL", model),
            ("OPENCODE_MODELS_PATH", MODEL_SNAPSHOT),
        ])
        .expect("the supervisor should start");
    let pipe = supervisor
        .take_stderr()
        .expect("the supervisor's diagnostics should be piped");

    (
        instance,
        supervisor,
        Diagnostics::pumped("the supervisor", pipe),
    )
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

async fn a_session(kestrel: &Kestrel) -> Session {
    let organization = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(&organization, "kestrel", &[], "main")
        .await;
    kestrel
        .declare_agent(&organization, "builder", "opencode", Some(MODEL))
        .await;

    kestrel.open_session("acme", "kestrel", "builder").await
}

/// Answering a turn never ends a Run, so one that answered is stopped, the way a person would.
async fn ended(kestrel: &Kestrel, run: RunId) -> Run {
    kestrel.after_one_turn_within(run, PATIENCE).await
}

/// The supervisor says how it answered a permission request only once the turn is over, so
/// what the model has been asked for is the only sight of a turn still in flight.
async fn working_at_a_turn(model: &Model, times: usize) {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    while model.times_working_at_the_rest_of_a_turn() < times {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the harness never came back for the rest of a turn"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn transcript(kestrel: &Kestrel, session: &Session) -> Vec<String> {
    kestrel
        .transcript(session.id)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect()
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn a_run_drives_the_harness_through_a_turn_and_ends_with_an_exit_status() {
    let kestrel = Kestrel::boot_reachable_from_an_environment().await;
    let model = Model::serving();
    let mut driven = Driven::in_an_environment(&kestrel, &model).await;

    let ended = ended(&kestrel, driven.run.id).await;

    driven.diagnostics.drain();
    assert_eq!(
        ended.exit,
        Some(Exit::Succeeded),
        "the supervisor said:\n{}",
        driven.diagnostics.everything_it_said()
    );
    assert!(
        !model.asked().is_empty(),
        "the run ended without the harness having reached a model at all"
    );
    assert!(
        driven.diagnostics.said(&format!("on the model {MODEL}")),
        "the run never set the model its agent named. the supervisor said:\n{}",
        driven.diagnostics.everything_it_said()
    );

    driven.destroy();
    kestrel.teardown().await;
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn what_the_agent_says_reaches_the_transcript_and_what_it_does_inside_the_run_does_not() {
    let kestrel = Kestrel::boot_reachable_from_an_environment().await;
    let model = Model::serving();
    let mut driven = Driven::in_an_environment(&kestrel, &model).await;
    let session = kestrel.show_session(driven.run.session).await;

    ended(&kestrel, driven.run.id).await;
    driven.diagnostics.drain();

    let transcript = transcript(&kestrel, &session).await;
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
        "the supervisor said:\n{}",
        driven.diagnostics.everything_it_said()
    );

    let transcript = transcript.join("\n");
    for inside_the_run in ["call-1", "shell", MARK] {
        assert!(
            !transcript.contains(inside_the_run),
            "the transcript carries {inside_the_run}, which happened inside the run:\n{transcript}"
        );
    }

    driven.destroy();
    kestrel.teardown().await;
}

/// The model serves the rest of the turn only once its tool call has been answered, so a
/// transcript carrying the second message is a round-trip the Harness came back from.
#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn a_permission_request_is_answered_and_the_harness_proceeds() {
    let kestrel = Kestrel::boot_reachable_from_an_environment().await;
    let model = Model::serving();
    let mut driven = Driven::in_an_environment(&kestrel, &model).await;
    let session = kestrel.show_session(driven.run.session).await;

    driven
        .diagnostics
        .wait_until_it_says("allowed once  tool call call-1")
        .await;
    let ended = ended(&kestrel, driven.run.id).await;

    assert_eq!(ended.exit, Some(Exit::Succeeded));
    assert!(
        transcript(&kestrel, &session)
            .await
            .iter()
            .any(|entry| entry.ends_with("a second message")),
        "the harness was answered and never went on"
    );

    driven.destroy();
    kestrel.teardown().await;
}

#[tokio::test]
#[ignore = "builds and runs the kestrel-env image"]
async fn a_harness_that_dies_mid_run_ends_the_run_with_an_exit_status() {
    let kestrel = Kestrel::boot_reachable_from_an_environment().await;
    let model = Model::dawdling();
    let mut driven = Driven::in_an_environment(&kestrel, &model).await;

    // Killed twice, because opencode can resume its session and is brought back from the first.
    working_at_a_turn(&model, 1).await;
    driven.kill_the_harness();
    working_at_a_turn(&model, 2).await;
    driven.kill_the_harness();

    let ended = ended(&kestrel, driven.run.id).await;
    let Some(Exit::Failed { because }) = &ended.exit else {
        panic!(
            "the run ended {:?}, and its harness was killed mid-turn",
            ended.exit
        );
    };
    assert!(!because.is_empty(), "the run failed without saying why");

    driven.destroy();
    kestrel.teardown().await;
}
