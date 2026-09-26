//! The installed `kestrel` Client against a control plane booted as its own binary: nothing
//! here reaches the database except through the operator boundary.

mod support;

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use support::client::{self, Finished, Invocation};
use support::github_stub::{self, GithubStub};
use support::scripted_agent::Script;
use tempfile::TempDir;

const PATIENCE: Duration = Duration::from_secs(30);

struct Kestrel {
    data_dir: TempDir,
}

/// A control plane running over a [`Kestrel`]'s data directory, and every line it has said.
struct Booted {
    child: Child,
    said: Arc<Mutex<String>>,
    operator: String,
}

impl Kestrel {
    fn new() -> Self {
        Self {
            data_dir: TempDir::new().expect("a temporary data directory"),
        }
    }

    /// An ephemeral link port, so tests that boot one concurrently never race over kestrel's
    /// default.
    fn boot(&self) -> Booted {
        self.booting("127.0.0.1:0", Script::Speaks, "info")
    }

    fn booting(&self, listen: &str, script: Script, level: &str) -> Booted {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kestrel-control-plane"))
            .env("KESTREL_DATA_DIR", self.data_dir.path())
            .env("KESTREL_LISTEN", listen)
            .env("KESTREL_OPERATOR_LISTEN", "127.0.0.1:0")
            .env("KESTREL_COMPUTE", "local-exec")
            .env("KESTREL_SUPERVISOR", support::supervisor::binary())
            .env(
                "KESTREL_AGENT_RUNTIME",
                format!(
                    "{}={}",
                    support::RUNTIME,
                    support::scripted_agent::playing(script)
                ),
            )
            .env("RUST_LOG", level)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the control plane should spawn");

        // Drained as it is said, so a chatty log cannot block the process on a full pipe.
        let stderr = child.stderr.take().expect("stderr should be piped");
        let said = Arc::new(Mutex::new(String::new()));
        let draining = Arc::clone(&said);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut said = draining.lock().expect("the log should not be poisoned");
                said.push_str(&line);
                said.push('\n');
            }
        });
        let operator = operator_of(&said);

        Booted {
            child,
            said,
            operator,
        }
    }
}

impl Booted {
    fn client(&self, args: &[&str]) -> Finished {
        client::ran(&self.operator, args)
    }

    fn client_as(&self, args: &[&str], invocation: Invocation) -> Finished {
        client::ran_as(&self.operator, args, invocation)
    }

    fn run(&self, args: &[&str]) -> String {
        succeeded(args, &self.client(args))
    }

    fn run_as(&self, args: &[&str], invocation: Invocation) -> String {
        succeeded(args, &self.client_as(args, invocation))
    }

    fn records(&self, args: &[&str]) -> Vec<Value> {
        self.client(args).records()
    }

    fn record(&self, args: &[&str]) -> Value {
        let mut records = self.records(args);
        assert_eq!(
            records.len(),
            1,
            "`kestrel {}` answered {records:?}",
            args.join(" ")
        );
        records.remove(0)
    }

    /// The refusal itself, so a test asserting one never passes on a command that succeeded.
    fn refused(&self, args: &[&str]) -> String {
        refusal(args, &self.client(args))
    }

    fn refused_as(&self, args: &[&str], invocation: Invocation) -> String {
        refusal(args, &self.client_as(args, invocation))
    }

    fn until(&self, args: &[&str], listed: impl Fn(&[Value]) -> bool, what: &str) -> Vec<Value> {
        let deadline = Instant::now() + PATIENCE;

        loop {
            let shown = self.records(args);
            if listed(&shown) {
                return shown;
            }
            assert!(
                Instant::now() < deadline,
                "`kestrel {}` never {what}. the last listing was:\n{shown:?}",
                args.join(" ")
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn said(&self) -> String {
        self.said
            .lock()
            .expect("the log should not be poisoned")
            .clone()
    }

    /// `Child::kill` is a `SIGKILL`, so nothing the control plane holds in memory is given a
    /// chance to land.
    fn killed(mut self) {
        let _ = self.child.kill();
        self.child
            .wait()
            .expect("the control plane should be waitable");
    }

    /// Signalled rather than killed, so the work role sees a stopped Run's supervisor off the
    /// link before it goes.
    fn terminated(mut self) {
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        self.child
            .wait()
            .expect("the control plane should be waitable");
    }
}

impl Drop for Booted {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn succeeded(args: &[&str], finished: &Finished) -> String {
    assert!(
        finished.status.success(),
        "`kestrel {}` failed with {}:\n{}",
        args.join(" "),
        finished.status,
        finished.err
    );
    finished.out.join("\n")
}

fn refusal(args: &[&str], finished: &Finished) -> String {
    assert!(
        !finished.status.success(),
        "`kestrel {}` was expected to be refused, and succeeded",
        args.join(" ")
    );
    finished.err.clone()
}

fn operator_of(said: &Mutex<String>) -> String {
    let deadline = Instant::now() + PATIENCE;

    loop {
        let started = said
            .lock()
            .expect("the log should not be poisoned")
            .lines()
            .find(|line| line.contains("role started") && line.contains("operator="))
            .map(str::to_owned);
        if let Some(line) = started {
            let address = line
                .split_whitespace()
                .find_map(|field| field.strip_prefix("operator="))
                .expect("the line should name the operator address");
            return format!("http://{address}");
        }
        assert!(
            Instant::now() < deadline,
            "the control plane never said where operators reach it:\n{}",
            said.lock().expect("the log should not be poisoned")
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A port nothing is listening on, so a control plane that is killed comes back on the
/// address the Environment it left behind already dialled.
fn a_free_port() -> String {
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("a bound address")
        .port();

    format!("127.0.0.1:{port}")
}

const RUN: &str = "id,state,exit,instance,worked_model";

fn runs(kestrel: &Booted, session: &str) -> Vec<Value> {
    kestrel.records(&["run", "list", "--session", session, "--json", RUN])
}

/// Answering a turn never ends a Run, so one waiting is stopped the way a person would.
fn dispatched(kestrel: &Booted, session: &str) -> Vec<Value> {
    let deadline = Instant::now() + PATIENCE;

    loop {
        let listed = runs(kestrel, session);
        if listed.iter().all(|run| !run["exit"]["status"].is_null()) {
            return listed;
        }
        for waiting in listed.iter().filter(|run| run["state"] == "waiting") {
            let run = waiting["id"].as_str().expect("a run's identifier");
            kestrel.run(&["run", "stop", run]);
        }
        assert!(
            Instant::now() < deadline,
            "no run in the session {session} ended. the last listing was:\n{listed:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn declared(kestrel: &Booted) {
    kestrel.run(&["organization", "declare", "acme"]);
    kestrel.run(&[
        "project",
        "declare",
        support::repository::NAME,
        "--repository",
        support::repository::url(),
        "--branch",
        support::repository::BRANCH,
    ]);
    kestrel.run(&[
        "agent",
        "declare",
        "builder",
        "--model",
        kestrel_scripted_agent::OTHER_MODEL,
    ]);
    kestrel.run_as(
        &["credential", "set", support::PROVIDER_KEY],
        Invocation::default().given(support::A_PROVIDER_KEY),
    );
}

fn opened(kestrel: &Booted) -> String {
    kestrel.run(&[
        "session",
        "open",
        "--project",
        support::repository::NAME,
        "--agent",
        "builder",
    ])
}

/// Each entry as `seq kind …`, without the moment it was appended, which is different every
/// run.
fn transcribed(kestrel: &Booted, session: &str) -> Vec<String> {
    kestrel
        .records(&["session", "transcript", session, "--json", "seq,entry"])
        .iter()
        .map(|recorded| {
            let entry = &recorded["entry"];
            let said = match entry["kind"].as_str().expect("an entry kind") {
                "participant_joined" => format!("participant joined {}", entry["participant"]),
                "run_started" => format!("run started {}", entry["run"]),
                "said" => format!("said {} {}", entry["participant"], entry["message"]),
                "run_ended" => format!("run ended {} {}", entry["run"], entry["exit"]["status"]),
                "instance_released" => format!(
                    "instance released {} {}",
                    entry["participant"], entry["instance"]
                ),
                kind => kind.to_owned(),
            };
            format!("{} {}", recorded["seq"], said.replace('"', ""))
        })
        .collect()
}

#[test]
fn a_role_boots_on_an_empty_data_directory_and_makes_its_database() {
    let kestrel = Kestrel::new();

    let booted = kestrel.boot();

    assert!(kestrel.data_dir.path().join("kestrel.db").exists());
    assert_eq!(booted.run(&["organization", "list"]), "");
}

#[test]
fn a_disabled_trigger_shows_its_reason_and_budget() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);
    booted.run(&[
        "trigger",
        "declare",
        "ready",
        "--filter",
        r#"{"exact": {"type": "com.github.issues.labeled"}}"#,
        "--brief",
        "Work on {{ event.data.issue.title }}",
        "--project",
        "kestrel",
        "--agent",
        "builder",
    ]);
    booted.run(&["trigger", "disable", "ready"]);

    let trigger = booted.record(&[
        "trigger",
        "show",
        "ready",
        "--json",
        "state,disabled_because,firing_budget",
    ]);

    assert_eq!(trigger["state"], "disabled:operator");
    assert_eq!(trigger["disabled_because"], "disabled by an operator");
    assert_eq!(trigger["firing_budget"]["limit"], 10);
}

#[test]
fn an_agents_model_changes_without_declaring_the_agent_again() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    booted.run(&["organization", "declare", "acme"]);
    booted.run(&[
        "agent",
        "declare",
        "builder",
        "--runtime",
        "codex",
        "--model",
        "claude-opus-5",
    ]);

    assert_eq!(
        booted.run(&["agent", "model", "builder", "--model", "claude-sonnet-5"]),
        "claude-sonnet-5"
    );
    assert_eq!(
        booted.records(&["agent", "list", "--json", "name,runtime,model"]),
        [serde_json::json!({ "name": "builder", "runtime": "codex", "model": "claude-sonnet-5" })]
    );
    assert_eq!(
        booted.record(&["agent", "model", "builder", "--json", "model"])["model"],
        Value::Null
    );
    assert!(
        booted
            .refused(&["agent", "model", "reviewer", "--model", "claude-sonnet-5"])
            .contains("no agent named reviewer"),
    );
}

#[test]
fn an_instance_is_shown_on_its_session_and_released_on_the_record() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);
    let session = opened(&booted);
    booted.run(&["run", "enqueue", "--session", &session]);
    dispatched(&booted, &session);

    let shown = booted.record(&["session", "show", &session, "--json", "instance,held"]);
    let instance = shown["instance"]
        .as_str()
        .expect("the session keeps its instance")
        .to_owned();
    assert_eq!(
        shown["held"],
        Value::Null,
        "a checkout the remote can restore was held"
    );
    assert_eq!(booted.run(&["instance", "list"]), "");

    assert_eq!(booted.run(&["instance", "release", &session]), instance);

    assert_eq!(
        booted.record(&["session", "show", &session, "--json", "instance"])["instance"],
        Value::Null,
        "a released instance is still the session's"
    );
    assert_eq!(
        transcribed(&booted, &session).last(),
        Some(&format!("6 instance released operator {instance}")),
        "the release is not on the record"
    );
    assert!(
        booted
            .refused(&["instance", "release", &session])
            .contains("no instance"),
    );
}

#[test]
fn a_run_ends_succeeded_while_waiting_and_is_not_stopped_twice() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);
    let session = opened(&booted);
    let run = booted.run(&["run", "enqueue", "--session", &session]);

    let listed = dispatched(&booted, &session);

    assert_eq!(listed[0]["id"], run);
    assert_eq!(listed[0]["exit"]["status"], "succeeded");
    assert!(
        listed[0]["instance"]
            .as_str()
            .is_some_and(|instance| instance.starts_with("local-exec/")),
        "the run does not list the instance it executed on: {listed:?}"
    );
    assert_eq!(
        listed[0]["worked_model"],
        kestrel_scripted_agent::OTHER_MODEL
    );
    assert!(
        booted
            .refused(&["run", "stop", &run])
            .contains("has already ended"),
    );
}

/// ADR-0002's definition of done for rung 0.1, out of process and against a real `SIGKILL`:
/// nothing the control plane held in memory lands, and the Environment it provisioned
/// outlives it.
#[test]
fn a_control_plane_killed_mid_turn_comes_back_and_the_turn_is_answered() {
    let kestrel = Kestrel::new();
    let listen = a_free_port();
    let killed = kestrel.booting(&listen, Script::Lingers, "info");
    declared(&killed);
    let session = opened(&killed);
    let run = killed.run(&["run", "enqueue", "--session", &session]);
    // The transcript says the Run started only once the supervisor holds the Start instruction,
    // which is the first moment a restart has anything to recover; an instance alone is not.
    killed.until(
        &["session", "transcript", &session, "--json", "seq,entry"],
        |transcribed| {
            transcribed
                .iter()
                .any(|recorded| recorded["entry"]["kind"] == "run_started")
        },
        "started its turn",
    );
    killed.killed();

    let restarted = kestrel.booting(&listen, Script::Lingers, "info");
    restarted.until(
        &["run", "list", "--session", &session, "--json", RUN],
        |listed| {
            listed
                .iter()
                .any(|run| run["state"] == "waiting" || !run["exit"].is_null())
        },
        "answered its turn",
    );
    restarted.run(&["run", "stop", &run]);
    let listed = runs(&restarted, &session);
    let transcript = transcribed(&restarted, &session);
    // The supervisor belongs to the control plane that was killed, so this one cannot wait it off
    // the link on the way down; it leaves within a poll of the stop reaching it.
    std::thread::sleep(Duration::from_secs(1));
    restarted.terminated();

    assert_eq!(
        listed[0]["exit"]["status"], "succeeded",
        "the run's turn was not answered after the restart: {listed:?}"
    );
    assert_eq!(
        transcript,
        vec![
            "1 participant joined builder".to_owned(),
            format!("2 run started {run}"),
            "3 said builder half of one message, and the other half".to_owned(),
            "4 said builder a second message".to_owned(),
            format!("5 run ended {run} succeeded"),
        ]
    );
}

/// A Client hands the control plane every secret it holds over the operator boundary, and
/// the control plane says none of them back while it takes them.
#[test]
fn secrets_set_through_the_client_appear_in_no_log_line() {
    let kestrel = Kestrel::new();
    let stub = GithubStub::start();
    let booted = kestrel.booting("127.0.0.1:0", Script::Speaks, "trace");
    let provider_key = "sk-kestrel-should-never-say-this-either";
    let signing_secret = "whsec-kestrel-should-never-say-this";

    let mut printed = booted.run(&["organization", "declare", "acme"]);
    printed += &booted.run_as(
        &["credential", "set", "ANTHROPIC_API_KEY"],
        Invocation::default().given(provider_key),
    );
    let registering = [
        "integration",
        "register",
        "github",
        "hub",
        "--repository",
        "jtmthf/kestrel",
        "--token",
        support::TOKEN,
        "--api",
        &stub.base_url(),
        "--webhook-secret",
        signing_secret,
    ];
    printed += &booted.run(&registering);
    printed += &booted.refused(&registering);
    printed += &booted.run(&["credential", "list"]);
    printed += &booted.run(&["integration", "list"]);
    let said = booted.said();
    booted.killed();

    assert!(
        said.contains("INSERT INTO provider_credential")
            && said.contains("INSERT INTO integration"),
        "the control plane logged nothing of what it was asked to hold:\n{said}"
    );
    for (name, secret) in [
        ("the provider key", provider_key),
        ("the token", support::TOKEN),
        ("the signing secret", signing_secret),
    ] {
        assert!(!printed.contains(secret), "the client printed {name}");
        assert!(!said.contains(secret), "a log line spelled {name} out");
    }
}

fn watching(kestrel: &Booted, stub: &GithubStub, interval: &str) {
    kestrel.run(&["organization", "declare", "acme"]);
    kestrel.run(&[
        "integration",
        "register",
        "github",
        "hub",
        "--repository",
        "jtmthf/kestrel",
        "--token",
        support::TOKEN,
        "--api",
        &stub.base_url(),
        "--interval",
        interval,
    ]);
}

/// The one command that has a credential in it, and the whole of what kestrel says while it
/// uses it: neither the listing an operator reads nor the log they debug from has the token.
#[test]
fn the_client_lists_what_a_poll_recorded_and_the_credential_appears_in_neither_it_nor_a_log() {
    let kestrel = Kestrel::new();
    let stub = GithubStub::start();
    for _ in 0..8 {
        stub.script(github_stub::page(&[github_stub::labelled(
            7,
            43,
            "ready-for-agent",
        )]));
    }
    let booted = kestrel.booting("127.0.0.1:0", Script::Speaks, "trace");
    watching(&booted, &stub, "1ms");

    let listed = booted.until(
        &["event", "list", "--json", "record,event"],
        |listed| !listed.is_empty(),
        "listed an event polled from github",
    );
    let said = booted.said();
    let record = listed[0]["record"].as_str().expect("an event record");
    let shown = booted.record(&["event", "show", record, "--json", "record,event"]);
    booted.killed();

    assert_eq!(shown["record"], record);
    assert_eq!(shown["event"]["id"], "7");
    assert_eq!(shown["event"]["specversion"], "1.0");
    assert_eq!(shown["event"]["subject"], "#43");
    let listed = serde_json::to_string(&listed).expect("the listing serializes");
    assert!(
        listed.contains("ready-for-agent"),
        "an event listing that does not say what happened:\n{listed}"
    );
    assert!(
        !listed.contains(support::TOKEN),
        "the listing spelled the credential out"
    );
    assert!(
        !said.contains(support::TOKEN),
        "a log line spelled the credential out"
    );
    assert!(
        said.contains("a poll recorded events"),
        "the control plane never said it polled:\n{said}"
    );
}

#[test]
fn a_dispatch_starts_a_triggers_work_on_the_issue_it_names() {
    let kestrel = Kestrel::new();
    let stub = GithubStub::start();
    stub.script_answer("GET", "/issues/60", github_stub::issue(60, &[]));
    let booted = kestrel.boot();
    declared(&booted);
    booted.run(&[
        "integration",
        "register",
        "github",
        "hub",
        "--repository",
        "jtmthf/kestrel",
        "--token",
        support::TOKEN,
        "--api",
        &stub.base_url(),
        "--carries",
        "outbound",
    ]);
    booted.run(&[
        "trigger",
        "declare",
        "delegated",
        "--filter",
        r#"{"exact": {"type": "com.github.issue_comment.created"}}"#,
        "--brief",
        "{{ instruction }} {{ event.data.issue.number }}",
        "--branch",
        "kestrel/issue-{{ event.data.issue.number }}",
        "--project",
        "kestrel",
        "--agent",
        "builder",
    ]);

    let tested = booted.record(&[
        "trigger",
        "test",
        "delegated",
        "--integration",
        "hub",
        "--issue",
        "60",
        "--instruction",
        "/tdd the parser",
        "--json",
        "matches,brief,branch,agent",
    ]);
    assert!(
        booted
            .records(&["event", "list", "--json", "record"])
            .is_empty()
    );

    let fired = booted.record(&[
        "trigger",
        "dispatch",
        "delegated",
        "--integration",
        "hub",
        "--issue",
        "60",
        "--instruction",
        "/tdd the parser",
        "--json",
        "outcome,session,run",
    ]);

    assert_eq!(
        tested,
        serde_json::json!({
            "matches": true,
            "brief": "/tdd the parser 60",
            "branch": "kestrel/issue-60",
            "agent": "builder",
        })
    );
    assert_eq!(fired["outcome"], "opened");
    let session = fired["session"].as_str().expect("the session it opened");
    let shown = booted.record(&["session", "show", session, "--json", "checkout"]);
    assert_eq!(shown["checkout"]["branch"], "kestrel/issue-60");
    for misused in [
        &["trigger", "test", "delegated", "--issue", "60"][..],
        &[
            "trigger",
            "test",
            "delegated",
            "--integration",
            "hub",
            "--issue",
            "60",
            "--event",
            "x",
        ],
    ] {
        assert_eq!(
            booted.client(misused).status.code(),
            Some(2),
            "`kestrel {}` is a usage error",
            misused.join(" ")
        );
    }
    assert!(
        booted
            .refused(&[
                "trigger",
                "dispatch",
                "delegated",
                "--integration",
                "nowhere",
                "--issue",
                "60",
            ])
            .contains("no integration named nowhere"),
    );
}

const APPLIED: &str = r#"
triggers:
  ready:
    filter: {exact: {type: com.github.issues.labeled}}
    brief: Work on {{ event.data.issue.title }}
    project: kestrel
    agent: builder
  triage:
    filter:
      all:
        - exact: {type: com.github.issues.opened}
        - exact: {data.issue.author_association: MEMBER}
    brief: Triage {{ event.data.issue.title }}
    project: kestrel
    agent: builder
"#;

fn applying(kestrel: &Booted, declarations: &str, flags: &[&str]) -> Finished {
    let mut args = vec!["trigger", "apply", "-f", "triggers.yaml"];
    args.extend_from_slice(flags);
    kestrel.client_as(
        &args,
        Invocation::default().file("triggers.yaml", declarations),
    )
}

/// What it printed, and what it warned of.
fn applied(kestrel: &Booted, declarations: &str, flags: &[&str]) -> (String, String) {
    let finished = applying(kestrel, declarations, flags);
    let diff = succeeded(&["trigger", "apply"], &finished);
    (diff, finished.err)
}

fn trigger_names(kestrel: &Booted) -> Vec<String> {
    kestrel
        .records(&["trigger", "list", "--json", "name"])
        .iter()
        .map(|trigger| trigger["name"].as_str().expect("a name").to_owned())
        .collect()
}

#[test]
fn apply_prints_the_diff_it_makes_and_nothing_once_it_is_made() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);

    let (diff, _) = applied(&booted, APPLIED, &[]);

    assert!(diff.contains("+ ready\n"), "{diff}");
    assert!(diff.contains("+ triage\n"), "{diff}");
    assert!(
        diff.contains("    brief\n      + Work on {{ event.data.issue.title }}\n"),
        "{diff}"
    );
    assert_eq!(trigger_names(&booted), ["ready", "triage"]);
    assert_eq!(
        booted.record(&["trigger", "show", "ready", "--json", "applied"])["applied"],
        true
    );

    assert_eq!(applied(&booted, APPLIED, &[]).0, "no changes");
}

#[test]
fn reapplying_changes_and_removes_only_what_a_file_applied() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);
    booted.run(&[
        "trigger",
        "declare",
        "one-off",
        "--filter",
        r#"{"exact": {"type": "com.example.build.failed"}}"#,
        "--brief",
        "Fix the build",
        "--project",
        "kestrel",
        "--agent",
        "builder",
    ]);
    applied(&booted, APPLIED, &[]);

    let changed = r#"
triggers:
  ready:
    filter: {exact: {type: com.github.issues.labeled}}
    brief: Work {{ event.data.issue.html_url }}
    project: kestrel
    agent: builder
"#;
    let (diff, _) = applied(&booted, changed, &[]);

    assert_eq!(
        diff,
        "~ ready\n    brief\n      - Work on {{ event.data.issue.title }}\n      + Work {{ event.data.issue.html_url }}\n- triage"
    );
    assert_eq!(trigger_names(&booted), ["one-off", "ready"]);
}

#[test]
fn a_one_off_a_file_declares_becomes_the_files() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);
    booted.run(&[
        "trigger",
        "declare",
        "ready",
        "--filter",
        r#"{"exact": {"type": "com.github.issues.labeled"}}"#,
        "--brief",
        "Work on {{ event.data.issue.title }}",
        "--project",
        "kestrel",
        "--agent",
        "builder",
    ]);

    let (diff, _) = applied(&booted, APPLIED, &[]);

    assert!(
        diff.contains("~ ready\n    declared by\n      - flags\n      + a file\n"),
        "{diff}"
    );
    applied(&booted, "triggers: {}", &[]);
    assert!(trigger_names(&booted).is_empty());
}

#[test]
fn a_dry_run_prints_the_diff_and_changes_nothing() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);

    let (diff, warned) = applied(&booted, APPLIED, &["--dry-run"]);

    assert!(diff.contains("+ ready\n"), "{diff}");
    assert!(trigger_names(&booted).is_empty());
    assert!(warned.contains("the trigger ready fires"), "{warned}");
    assert!(warned.contains("0.4"), "{warned}");
    assert!(!warned.contains("the trigger triage"), "{warned}");
}

#[test]
fn an_apply_that_cannot_be_made_whole_changes_nothing() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);
    let naming_a_stranger = format!(
        "{APPLIED}  stranger:\n    filter: {{exact: {{type: x}}}}\n    brief: x\n    project: kestrel\n    agent: nobody\n"
    );

    let refusal = refusal(
        &["trigger", "apply"],
        &applying(&booted, &naming_a_stranger, &[]),
    );

    assert!(refusal.contains("nobody"), "{refusal}");
    assert!(trigger_names(&booted).is_empty());
}

#[test]
fn a_declaration_file_that_is_not_one_is_refused_saying_where() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);

    for (declarations, because) in [
        ("", "triggers"),
        ("triggers:\n  ready:\n    brief: x\n", "filter"),
        (
            "triggers:\n  ready:\n    filter: {sql: x}\n    brief: x\n    project: kestrel\n    agent: builder\n",
            "the trigger ready",
        ),
        (
            "triggers:\n  ready:\n    filter: {exact: {type: x}}\n    brief: x\n    correlation: x\n    project: kestrel\n    agent: builder\n",
            "misses",
        ),
        (
            "triggers:\n  ready:\n    filter: {exact: {type: x}}\n    brief: x\n    agnet: builder\n    project: kestrel\n",
            "agnet",
        ),
    ] {
        let said = refusal(&["trigger", "apply"], &applying(&booted, declarations, &[]));
        assert!(
            said.contains(because),
            "{declarations:?} was refused unhelpfully: {said}"
        );
    }
}

#[test]
fn apply_reads_a_declaration_file_from_standard_input() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);

    booted.run_as(
        &["trigger", "apply", "-f", "-"],
        Invocation::default().given(APPLIED),
    );

    assert_eq!(trigger_names(&booted), ["ready", "triage"]);
}

#[test]
fn a_trigger_is_tested_as_a_file_declares_it_rather_than_as_it_was_applied() {
    let kestrel = Kestrel::new();
    let stub = GithubStub::start();
    stub.script(github_stub::page(&[github_stub::labelled(
        7,
        43,
        "ready-for-agent",
    )]));
    let booted = kestrel.boot();
    declared(&booted);
    watching(&booted, &stub, "1ms");
    booted.run(&[
        "trigger",
        "declare",
        "ready",
        "--filter",
        r#"{"exact": {"type": "com.github.issues.opened"}}"#,
        "--brief",
        "as applied",
        "--project",
        "kestrel",
        "--agent",
        "builder",
    ]);
    let event = booted.until(
        &["event", "list", "--json", "record"],
        |listed| !listed.is_empty(),
        "listed an event polled from github",
    )[0]["record"]
        .as_str()
        .expect("an event record")
        .to_owned();
    let declaring = "triggers:\n  ready:\n    filter: {exact: {type: com.github.issues.labeled}}\n    brief: '{{ instruction }} {{ event.subject }}'\n    project: kestrel\n    agent: builder\n";

    let tested = booted
        .client_as(
            &[
                "trigger",
                "test",
                "ready",
                "--event",
                &event,
                "-f",
                "triggers.yaml",
                "--instruction",
                "@instruction.md",
                "--json",
                "matches,brief",
            ],
            Invocation::default()
                .file("triggers.yaml", declaring)
                .file("instruction.md", "/triage"),
        )
        .records();

    assert_eq!(
        tested,
        [serde_json::json!({ "matches": true, "brief": "/triage #43" })]
    );
    assert_eq!(
        booted.record(&[
            "trigger", "test", "ready", "--event", &event, "--json", "matches"
        ])["matches"],
        false
    );
    assert!(
        booted
            .refused_as(
                &[
                    "trigger",
                    "test",
                    "elsewhere",
                    "--event",
                    &event,
                    "-f",
                    "triggers.yaml"
                ],
                Invocation::default().file("triggers.yaml", declaring),
            )
            .contains("declares no trigger elsewhere"),
    );
}

#[test]
fn a_brief_and_a_filter_are_read_from_a_file_or_standard_input() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);

    let finished = booted.client_as(
        &[
            "trigger",
            "declare",
            "ready",
            "--filter",
            "-",
            "--brief",
            "@brief.md",
            "--project",
            "kestrel",
            "--agent",
            "builder",
        ],
        Invocation::default()
            .given(r#"{"exact": {"type": "com.github.issues.labeled"}}"#)
            .file(
                "brief.md",
                "Work on {{ event.data.issue.title }}\n\nand say so.\n",
            ),
    );
    succeeded(&["trigger", "declare"], &finished);

    let shown = booted.record(&["trigger", "show", "ready", "--json", "filter,brief,applied"]);
    assert_eq!(
        shown["filter"],
        serde_json::json!({ "exact": { "type": "com.github.issues.labeled" } })
    );
    assert!(
        shown["brief"]
            .as_str()
            .is_some_and(|brief| brief.ends_with("and say so.\n")),
        "{shown}"
    );
    assert_eq!(shown["applied"], false);
    assert!(
        finished.err.contains("the trigger ready fires"),
        "a one-off admitting outsiders was declared without a warning"
    );
}

#[test]
fn only_one_of_a_filter_and_a_brief_is_read_from_standard_input() {
    let kestrel = Kestrel::new();
    let booted = kestrel.boot();
    declared(&booted);

    let refusal = booted.refused(&[
        "trigger",
        "declare",
        "ready",
        "--filter",
        "-",
        "--brief",
        "-",
        "--project",
        "kestrel",
        "--agent",
        "builder",
    ]);

    assert!(refusal.contains("standard input"), "{refusal}");
}
