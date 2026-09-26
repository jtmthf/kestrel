//! Opt-in smoke checks that each harness in the `kestrel-dev` image reaches a real model on a
//! person's own subscription, then again after the control plane and the Instance are both
//! replaced (ADR-0025). Each reads its login from the variables it names and fails, rather than
//! passing vacuously, when they are unset; USAGE.md says how to run them.

mod support;

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use kestrel::domain::{Exit, Run, RunState, Session};
use kestrel::log;
use kestrel::profile::{Contents, Entry};
use support::Harness;
use support::image::{self, Container};

const PATIENCE: Duration = Duration::from_secs(300);
/// Asked for something the prompt does not contain, so an error the runtime echoes back as
/// its answer is never mistaken for a model's.
const PROMPT: &str = "Reply with the word harrier spelled backwards, in lowercase, and nothing else. Do not use any tools.";
const ANSWER: &str = "reirrah";
const ORGANIZATION: &str = "smoke";
const PROJECT: &str = "smoke";
const AGENT: &str = "smoke";
const PROFILE: &str = "smoke";

struct Subject {
    runtime: &'static str,
    command: &'static str,
    model: Option<String>,
    variables: Vec<(&'static str, String)>,
    files: Vec<Login>,
}

/// A login file read from the host, handed back there once the smoke is over so a refresh the
/// runtime made does not leave the person holding a token it revoked.
struct Login {
    path: &'static str,
    host: PathBuf,
    as_read: String,
}

fn required(variable: &str, what: &str) -> String {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("set {variable} to {what} to run this smoke check"))
}

#[tokio::test]
#[ignore = "calls a real model on a person's Codex subscription"]
async fn codex_answers_on_a_chatgpt_login_before_and_after_a_restart() {
    let host = PathBuf::from(required(
        "KESTREL_SMOKE_CODEX_AUTH",
        "the path of a file-backed Codex auth.json, which the refreshed login is written back to",
    ));
    let as_read = fs::read_to_string(&host).expect("the Codex login should read");

    smoke(Subject {
        runtime: "codex",
        command: "codex-acp",
        model: None,
        variables: Vec::new(),
        files: vec![Login {
            path: ".codex/auth.json",
            host,
            as_read,
        }],
    })
    .await;
}

#[tokio::test]
#[ignore = "calls a real model on a person's OpenCode Go subscription"]
async fn opencode_answers_on_an_opencode_go_key_before_and_after_a_restart() {
    let key = required("KESTREL_SMOKE_OPENCODE_API_KEY", "an OpenCode Go key");
    let model = required(
        "KESTREL_SMOKE_OPENCODE_MODEL",
        "an OpenCode Go model, such as opencode-go/glm-5.3",
    );
    assert!(
        model.starts_with("opencode-go/"),
        "{model} is not an OpenCode Go model, so it would not exercise the subscription"
    );

    smoke(Subject {
        runtime: "opencode",
        command: "opencode acp --print-logs",
        model: Some(model),
        variables: vec![("OPENCODE_API_KEY", key)],
        files: Vec::new(),
    })
    .await;
}

#[tokio::test]
#[ignore = "calls a real model on a person's Claude subscription"]
async fn claude_answers_on_a_claude_plan_token_before_and_after_a_restart() {
    let token = required(
        "KESTREL_SMOKE_CLAUDE_OAUTH_TOKEN",
        "a token `claude setup-token` printed",
    );

    smoke(Subject {
        runtime: "claude",
        command: "claude-agent-acp",
        model: None,
        variables: vec![("CLAUDE_CODE_OAUTH_TOKEN", token)],
        files: Vec::new(),
    })
    .await;
}

/// The login is handed back and the harness torn down before anything is asserted, so a
/// failing smoke still leaves the person's login where it found it.
async fn smoke(subject: Subject) {
    let harness = Harness::dispatching_runtimes_in(
        image::development(),
        &[(subject.runtime, subject.command)],
    )
    .await;
    declared(&harness, &subject).await;

    let first = attempt(&harness, &subject, Round::BeforeTheRestart).await;
    let (harness, second) = match &first {
        Ok(run) => {
            let harness = harness.kill_and_restart().await;
            if let Some(instance) = &run.instance {
                Container::named(instance).destroy();
            }
            let second = attempt(&harness, &subject, Round::AfterTheRestart).await;
            (harness, Some(second))
        }
        Err(_) => (harness, None),
    };
    handed_back(&harness, &subject).await;
    harness.teardown().await;

    let runtime = subject.runtime;
    let first = first.unwrap_or_else(|failure| panic!("{runtime} {failure}"));
    let second = second
        .expect("a second attempt follows a first that worked")
        .unwrap_or_else(|failure| panic!("{runtime} {failure}"));
    assert_ne!(
        first.instance, second.instance,
        "{runtime} answered after the restart on the Instance it answered on before it"
    );
}

async fn declared(harness: &Harness, subject: &Subject) {
    let organization = harness.declare_organization(ORGANIZATION).await;
    harness
        .declare_project(&organization, PROJECT, &[], "main")
        .await;
    harness
        .declare_agent(
            &organization,
            AGENT,
            subject.runtime,
            subject.model.as_deref(),
        )
        .await;
    harness
        .declare_profile(ORGANIZATION, PROFILE, "smoke")
        .await
        .expect("the profile should declare");
    for (name, value) in &subject.variables {
        let entry = Entry::variable(name).expect("a variable");
        harness
            .hold_in_profile(ORGANIZATION, PROFILE, &entry, value)
            .await;
    }
    for login in &subject.files {
        let entry = Entry::file(login.path).expect("a file");
        harness
            .hold_in_profile(ORGANIZATION, PROFILE, &entry, &login.as_read)
            .await;
    }
}

async fn attempt(harness: &Harness, subject: &Subject, round: Round) -> Result<Run, Failure> {
    let session = harness
        .open_session_with(ORGANIZATION, PROJECT, AGENT, PROFILE)
        .await;
    let run = harness.post(session.id, "operator", PROMPT).await;
    let (run, answered) = settled(harness, run).await;
    let said = said_by_the_agent(harness, &session).await;
    if run.state != RunState::Ended {
        harness.stop_run(run.id).await;
    }

    let failed = match &run.exit {
        Some(Exit::Failed { because }) => Some(because.clone()),
        _ => None,
    };
    if answered && failed.is_none() && said.to_lowercase().contains(ANSWER) {
        return Ok(run);
    }

    let evidence = format!(
        "{}\nthe agent said: {said}",
        match (&failed, answered) {
            (Some(because), _) => format!("the run failed: {because}"),
            (None, true) => format!("the agent answered without {ANSWER:?}"),
            (None, false) => format!("the agent had not answered within {PATIENCE:?}"),
        }
    );

    Err(Failure {
        round,
        problem: diagnosed(round, answered, &evidence),
        evidence: secrets(harness, subject).await.redacted(&evidence),
    })
}

/// Waited for without panicking, because a smoke that times out still hands its login back.
async fn settled(harness: &Harness, run: Run) -> (Run, bool) {
    let deadline = tokio::time::Instant::now() + PATIENCE;

    loop {
        let answered = harness
            .turns(run.id)
            .await
            .iter()
            .any(|turn| turn.answered_at.is_some());
        let run = harness.run(run.id).await;
        if answered || run.state == RunState::Ended || tokio::time::Instant::now() >= deadline {
            return (run, answered);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn said_by_the_agent(harness: &Harness, session: &Session) -> String {
    harness
        .transcript(session.id)
        .await
        .into_iter()
        .filter_map(|entry| match entry.entry {
            log::Entry::Said {
                participant,
                message,
            } if participant == session.agent.name => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Everything the profile held and holds now, because a login refreshed mid-smoke is as much a
/// secret as the one it replaced.
async fn held_now(harness: &Harness) -> Contents {
    let profile = harness
        .profiles(ORGANIZATION)
        .await
        .into_iter()
        .map(|(profile, _)| profile)
        .find(|profile| profile.name == PROFILE)
        .expect("the smoke's profile");

    harness.profile_contents(&profile).await
}

async fn secrets(harness: &Harness, subject: &Subject) -> Secrets {
    let now = held_now(harness).await;
    let held = subject
        .variables
        .iter()
        .map(|(_, value)| value.as_str())
        .chain(subject.files.iter().map(|login| login.as_read.as_str()))
        .chain(now.variables.values().map(String::as_str))
        .chain(now.files.values().map(String::as_str));

    Secrets::of(&held.collect::<Vec<_>>())
}

async fn handed_back(harness: &Harness, subject: &Subject) {
    let now = held_now(harness).await;

    for login in &subject.files {
        let Some(refreshed) = now.files.get(login.path) else {
            continue;
        };
        if *refreshed == login.as_read {
            continue;
        }
        let host = &login.host;
        if fs::read_to_string(host).ok().as_ref() != Some(&login.as_read) {
            eprintln!(
                "{} changed on the host during the smoke, so the login the runtime refreshed is not written over it",
                host.display()
            );
            continue;
        }
        let beside = host.with_extension("kestrel-smoke");
        write_privately(&beside, refreshed);
        fs::rename(&beside, host).expect("the refreshed login should replace the one read");
        eprintln!("handed the refreshed login back to {}", host.display());
    }
}

fn write_privately(path: &std::path::Path, contents: &str) {
    use std::io::Write as _;

    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .expect("the refreshed login should write");
}

struct Failure {
    round: Round,
    problem: Problem,
    evidence: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let when = match self.round {
            Round::BeforeTheRestart => "before the restart",
            Round::AfterTheRestart => "after the restart",
        };
        let problem = match self.problem {
            Problem::RuntimeLaunch => "a runtime launch problem",
            Problem::Authentication => "an authentication problem",
            Problem::Entitlement => "an entitlement problem",
            Problem::Persistence => "a persistence problem: the login worked before the restart",
            Problem::Unclassified => {
                "a problem that is none of launch, authentication, entitlement or persistence"
            }
        };

        write!(f, "failed {when} with {problem}\n{}", self.evidence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Round {
    BeforeTheRestart,
    AfterTheRestart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Problem {
    RuntimeLaunch,
    Authentication,
    Entitlement,
    Persistence,
    Unclassified,
}

/// Checked before authentication, because a refused plan can say 401 too.
const ENTITLEMENT: &[&str] = &[
    "only authorized for use with",
    "not included in your plan",
    "your plan does not",
    "usage limit",
    "forbidden",
    "403",
    "not entitled",
    "insufficient credits",
    "credit balance",
];
const AUTHENTICATION: &[&str] = &[
    "401",
    "unauthorized",
    "authentication_error",
    "authentication failed",
    "invalid api key",
    "invalid_api_key",
    "invalid_grant",
    "must be logged in",
    "not logged in",
    "login expired",
    "token expired",
    "refresh token",
    "/login",
    "auth_required",
    "authentication required",
    "failed to authenticate",
    "authenticates only at an interactive terminal",
];
const LAUNCH: &[&str] = &[
    "no such file",
    "command not found",
    "permission denied",
    "exec format error",
    "acp v1",
    "exited before",
    "names a runtime",
];

fn diagnosed(round: Round, answered: bool, evidence: &str) -> Problem {
    let evidence = evidence.to_lowercase();
    let says = |phrases: &[&str]| phrases.iter().any(|phrase| mentions(&evidence, phrase));

    if says(ENTITLEMENT) {
        Problem::Entitlement
    } else if says(AUTHENTICATION) {
        match round {
            Round::BeforeTheRestart => Problem::Authentication,
            Round::AfterTheRestart => Problem::Persistence,
        }
    } else if !answered && says(LAUNCH) {
        Problem::RuntimeLaunch
    } else {
        Problem::Unclassified
    }
}

/// A run id holds `401` as readily as a status line does.
fn mentions(text: &str, phrase: &str) -> bool {
    let bounded = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());

    text.match_indices(phrase).any(|(at, _)| {
        bounded(text[..at].chars().next_back()) && bounded(text[at + phrase.len()..].chars().next())
    })
}

struct Secrets(Vec<String>);

impl Secrets {
    /// A login file is redacted as a whole and token by token, because a runtime that logs one
    /// logs a token out of it rather than the file.
    fn of(values: &[&str]) -> Self {
        let mut secrets = Vec::new();
        for value in values {
            secrets.push((*value).to_owned());
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) {
                leaves(&parsed, &mut secrets);
            }
        }
        secrets.retain(|secret| secret.len() >= 8);
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();

        Self(secrets)
    }

    fn redacted(&self, text: &str) -> String {
        self.0.iter().fold(text.to_owned(), |text, secret| {
            text.replace(secret, "[redacted]")
        })
    }
}

fn leaves(value: &serde_json::Value, into: &mut Vec<String>) {
    match value {
        serde_json::Value::String(leaf) => into.push(leaf.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|item| leaves(item, into)),
        serde_json::Value::Object(fields) => fields.values().for_each(|field| leaves(field, into)),
        _ => {}
    }
}

#[test]
fn a_rejected_login_is_an_authentication_problem() {
    for evidence in [
        "API Error: 401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\"}}",
        "Invalid API key · Please run /login",
        "this agent must be logged in before it answers session/new",
        "Login expired · Please run /login",
        "Authentication required: provider authentication required: {}",
        "Failed to authenticate. API Error: 401 OAuth access token is invalid.",
    ] {
        assert_eq!(
            diagnosed(Round::BeforeTheRestart, false, evidence),
            Problem::Authentication,
            "{evidence}"
        );
    }
}

#[test]
fn a_login_the_plan_does_not_cover_is_an_entitlement_problem() {
    for evidence in [
        "This credential is only authorized for use with Claude Code and cannot be used for other API requests.",
        "You've hit your usage limit. Upgrade to Pro",
        "unexpected status 403 Forbidden",
        "the model opencode-go/glm-5.3 is not included in your plan",
    ] {
        assert_eq!(
            diagnosed(Round::BeforeTheRestart, false, evidence),
            Problem::Entitlement,
            "{evidence}"
        );
    }
}

#[test]
fn a_runtime_that_never_came_up_is_a_launch_problem() {
    for evidence in [
        "No such file or directory (os error 2)",
        "kestrel speaks ACP v1, and this agent answered v0",
        "sh: 1: codex-acp: command not found",
    ] {
        assert_eq!(
            diagnosed(Round::BeforeTheRestart, false, evidence),
            Problem::RuntimeLaunch,
            "{evidence}"
        );
    }
}

#[test]
fn a_login_refused_only_after_the_restart_is_a_persistence_problem() {
    assert_eq!(
        diagnosed(
            Round::AfterTheRestart,
            false,
            "Login expired · Please run /login"
        ),
        Problem::Persistence
    );
    assert_eq!(
        diagnosed(Round::AfterTheRestart, false, "No such file or directory"),
        Problem::RuntimeLaunch
    );
}

#[test]
fn identifiers_are_not_status_codes() {
    assert_eq!(
        diagnosed(
            Round::BeforeTheRestart,
            true,
            "the run 01a0acf7-3f4d-7952-a4a0-f401a181b089 answered pelican"
        ),
        Problem::Unclassified
    );
}

#[test]
fn a_held_secret_and_every_token_inside_one_are_redacted() {
    let login = r#"{"tokens":{"access_token":"eyJhbGciOiJSUzI1NiJ9.payload","refresh_token":"rt_abcdefghijkl"},"last_refresh":"2026"}"#;
    let secrets = Secrets::of(&["sk-go-0123456789", login]);

    let said = secrets.redacted(
        "key sk-go-0123456789 refused; refreshing with rt_abcdefghijkl; bearer eyJhbGciOiJSUzI1NiJ9.payload",
    );

    assert_eq!(
        said,
        "key [redacted] refused; refreshing with [redacted]; bearer [redacted]"
    );
}

#[test]
fn a_throttle_is_not_a_plan_refusal() {
    assert_eq!(
        diagnosed(
            Round::BeforeTheRestart,
            true,
            "429 Too Many Requests: rate limit exceeded, retry later"
        ),
        Problem::Unclassified
    );
}
