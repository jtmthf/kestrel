mod support;

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use support::scripted_agent::Script;
use tempfile::TempDir;

const PATIENCE: Duration = Duration::from_secs(30);

struct Kestrel {
    data_dir: TempDir,
}

impl Kestrel {
    fn new() -> Self {
        Self {
            data_dir: TempDir::new().expect("a temporary data directory"),
        }
    }

    fn try_run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_kestrel"))
            .args(args)
            .env("KESTREL_DATA_DIR", self.data_dir.path())
            .output()
            .expect("kestrel should run")
    }

    /// An ephemeral port, so tests that boot one concurrently never race over kestrel's
    /// default.
    fn boot(&self) -> Child {
        self.booting("127.0.0.1:0", Script::Speaks)
    }

    fn booting(&self, listen: &str, script: Script) -> Child {
        Command::new(env!("CARGO_BIN_EXE_kestrel"))
            .env("KESTREL_DATA_DIR", self.data_dir.path())
            .env("KESTREL_LISTEN", listen)
            .env("KESTREL_SUPERVISOR", support::supervisor::binary())
            .env(
                "KESTREL_AGENT_RUNTIME",
                support::scripted_agent::playing(script),
            )
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("kestrel should spawn")
    }

    fn until(&self, args: &[&str], listed: impl Fn(&str) -> bool, what: &str) -> String {
        let deadline = Instant::now() + PATIENCE;

        loop {
            let shown = self.run(args);
            if listed(&shown) {
                return shown;
            }
            assert!(
                Instant::now() < deadline,
                "`kestrel {}` never {what}. the last listing was:\n{shown}",
                args.join(" ")
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `Child::kill` is a `SIGKILL`, so nothing kestrel holds in memory is given a chance to land.
    fn kill_a_booted_control_plane(&self) {
        let mut kestrel = self.boot();

        let stderr = BufReader::new(kestrel.stderr.take().expect("stderr should be piped"));
        let booted = stderr
            .lines()
            .map_while(Result::ok)
            .any(|line| line.contains("role started"));

        let _ = kestrel.kill();
        kestrel.wait().expect("kestrel should be waitable");
        assert!(booted, "kestrel never started a role");
    }

    fn run(&self, args: &[&str]) -> String {
        let output = self.try_run(args);
        assert!(
            output.status.success(),
            "`kestrel {}` failed with {}:\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("output should be utf-8")
            .trim_end()
            .to_owned()
    }
}

/// The one path a person actually takes: the work role running while a Run is worked.
fn dispatched(kestrel: &Kestrel, session: &str) -> String {
    let mut booted = kestrel.boot();
    let deadline = Instant::now() + PATIENCE;

    let listed = loop {
        let listed = kestrel.run(&["run", "list", "--session", session]);
        if listed.contains("succeeded") || listed.contains("failed") {
            break listed;
        }
        assert!(
            Instant::now() < deadline,
            "no run in the session {session} ended. the last listing was:\n{listed}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    let _ = booted.kill();
    booted.wait().expect("kestrel should be waitable");

    listed
}

fn declared() -> Kestrel {
    let kestrel = Kestrel::new();
    kestrel.run(&["organization", "declare", "acme"]);
    kestrel.run(&[
        "workspace",
        "declare",
        "kestrel",
        "--organization",
        "acme",
        "--repository",
        "https://github.com/jtmthf/kestrel",
        "--branch",
        "main",
    ]);
    kestrel.run(&[
        "agent",
        "declare",
        "builder",
        "--organization",
        "acme",
        "--model",
        "claude-opus-5",
    ]);
    kestrel
}

fn shown(block: &str) -> HashMap<String, String> {
    block
        .lines()
        .map(|line| {
            let (field, value) = line.split_once(' ').expect("a field and a value");
            (field.to_owned(), value.trim_start().to_owned())
        })
        .collect()
}

/// The refusal itself, so a test asserting one never passes on a command that succeeded.
fn refused(kestrel: &Kestrel, args: &[&str]) -> String {
    let refusal = kestrel.try_run(args);
    assert!(
        !refusal.status.success(),
        "`kestrel {}` was expected to be refused, and succeeded",
        args.join(" ")
    );
    String::from_utf8_lossy(&refusal.stderr).into_owned()
}

fn opened(kestrel: &Kestrel) -> String {
    kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ])
}

#[test]
fn first_boot_needs_no_configuration_file_to_declare_an_organization() {
    let kestrel = Kestrel::new();

    let id = kestrel.run(&["organization", "declare", "acme"]);

    assert!(kestrel.data_dir.path().join("kestrel.db").exists());
    assert_eq!(
        kestrel.run(&["organization", "list"]),
        format!("{id}  acme")
    );
}

#[test]
fn a_role_boots_on_an_empty_data_directory_and_makes_its_database() {
    let kestrel = Kestrel::new();

    kestrel.kill_a_booted_control_plane();

    assert!(kestrel.data_dir.path().join("kestrel.db").exists());
    assert_eq!(kestrel.run(&["organization", "list"]), "");
}

#[test]
fn a_workspace_names_repositories_and_a_branch() {
    let kestrel = Kestrel::new();
    kestrel.run(&["organization", "declare", "acme"]);

    let id = kestrel.run(&[
        "workspace",
        "declare",
        "kestrel",
        "--organization",
        "acme",
        "--repository",
        "https://github.com/jtmthf/kestrel",
        "--repository",
        "https://github.com/jtmthf/skills",
        "--branch",
        "main",
    ]);

    assert_eq!(
        kestrel.run(&["workspace", "list", "--organization", "acme"]),
        format!(
            "{id}  kestrel  main  https://github.com/jtmthf/kestrel,https://github.com/jtmthf/skills"
        )
    );
}

#[test]
fn a_workspace_cannot_be_declared_against_an_organization_that_was_never_declared() {
    let kestrel = Kestrel::new();

    let refusal = kestrel.try_run(&[
        "workspace",
        "declare",
        "kestrel",
        "--organization",
        "acme",
        "--repository",
        "https://github.com/jtmthf/kestrel",
        "--branch",
        "main",
    ]);

    assert!(!refusal.status.success());
    assert!(
        String::from_utf8_lossy(&refusal.stderr).contains("no organization named acme"),
        "unhelpful refusal: {}",
        String::from_utf8_lossy(&refusal.stderr)
    );
}

#[test]
fn an_agent_names_the_runtime_and_model_it_participates_with() {
    let kestrel = Kestrel::new();
    kestrel.run(&["organization", "declare", "acme"]);

    let id = kestrel.run(&[
        "agent",
        "declare",
        "builder",
        "--organization",
        "acme",
        "--model",
        "claude-opus-5",
    ]);

    assert_eq!(
        kestrel.run(&["agent", "list", "--organization", "acme"]),
        format!("{id}  builder  opencode  claude-opus-5")
    );
}

#[test]
fn a_session_opens_against_a_workspace_and_an_agent() {
    let kestrel = declared();

    let id = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);
    let session = shown(&kestrel.run(&["session", "show", &id]));

    assert_eq!(session["session"], id);
    assert_eq!(session["organization"], "acme");
    assert_eq!(session["workspace"], "kestrel");
    assert_eq!(session["agent"], "builder");
    assert_eq!(session["state"], "open");
}

#[test]
fn opening_a_session_records_the_agent_joining_it() {
    let kestrel = declared();

    let id = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);
    let transcript = kestrel.run(&["session", "transcript", &id]);

    let entry = transcript.lines().next().expect("one entry");
    assert!(
        entry.starts_with("1  ") && entry.ends_with("participant joined  builder"),
        "unexpected first transcript entry: {entry}"
    );
    assert_eq!(transcript.lines().count(), 1);
}

#[test]
fn a_session_outlives_the_process_that_opened_it() {
    let kestrel = declared();
    let id = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);
    let session = kestrel.run(&["session", "show", &id]);
    let transcript = kestrel.run(&["session", "transcript", &id]);

    kestrel.kill_a_booted_control_plane();

    assert_eq!(kestrel.run(&["session", "show", &id]), session);
    assert_eq!(kestrel.run(&["session", "transcript", &id]), transcript);
}

#[test]
fn a_session_takes_one_run_at_a_time() {
    let kestrel = declared();
    let session = opened(&kestrel);
    let run = kestrel.run(&["run", "enqueue", "--session", &session]);

    let refusal = refused(&kestrel, &["run", "enqueue", "--session", &session]);

    assert!(
        refusal.contains(&run) && refusal.contains("one at a time"),
        "unhelpful refusal: {refusal}"
    );
    assert_eq!(
        kestrel
            .run(&["run", "list", "--session", &session])
            .lines()
            .count(),
        1
    );
}

#[test]
fn a_session_seals_through_the_cli_only_once_no_run_is_in_flight() {
    let kestrel = declared();
    let session = opened(&kestrel);
    let run = kestrel.run(&["run", "enqueue", "--session", &session]);

    let refusal = refused(&kestrel, &["session", "seal", &session]);
    assert!(
        refusal.contains(&run) && refusal.contains("still in flight"),
        "unhelpful refusal: {refusal}"
    );
    assert_eq!(
        shown(&kestrel.run(&["session", "show", &session]))["state"],
        "open"
    );

    dispatched(&kestrel, &session);

    assert_eq!(kestrel.run(&["session", "seal", &session]), session);
    let sealed = shown(&kestrel.run(&["session", "show", &session]));
    assert_eq!(sealed["state"], "sealed");
    assert!(
        sealed.contains_key("sealed"),
        "a sealed session says when: {sealed:?}"
    );
}

#[test]
fn a_sealed_session_is_readable_and_takes_no_more_work() {
    let kestrel = declared();
    let session = opened(&kestrel);
    kestrel.run(&["run", "enqueue", "--session", &session]);
    dispatched(&kestrel, &session);
    let transcript = kestrel.run(&["session", "transcript", &session]);

    kestrel.run(&["session", "seal", &session]);

    assert_eq!(
        kestrel.run(&["session", "transcript", &session]),
        transcript
    );
    assert!(
        transcript.lines().count() > 1,
        "nothing was transcribed to read back"
    );
    let refusal = refused(&kestrel, &["run", "enqueue", "--session", &session]);
    assert!(refusal.contains("sealed"), "unhelpful refusal: {refusal}");
}

#[test]
fn no_command_reopens_a_sealed_session() {
    let kestrel = declared();
    let session = opened(&kestrel);
    kestrel.run(&["session", "seal", &session]);
    let sealed = kestrel.run(&["session", "show", &session]);

    for again in [
        vec!["session", "seal", &session],
        vec!["run", "enqueue", "--session", &session],
    ] {
        refused(&kestrel, &again);
    }

    assert_eq!(kestrel.run(&["session", "show", &session]), sealed);
}

#[test]
fn work_that_continues_a_sealed_session_reads_the_link_from_both_ends() {
    let kestrel = declared();
    let sealed = opened(&kestrel);
    kestrel.run(&["session", "seal", &sealed]);

    let continuing = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
        "--continues",
        &sealed,
    ]);

    assert_ne!(continuing, sealed);
    assert_eq!(
        shown(&kestrel.run(&["session", "show", &continuing]))["continues"],
        sealed
    );
    assert_eq!(
        shown(&kestrel.run(&["session", "show", &sealed]))["continued-by"],
        continuing
    );
    kestrel.run(&["run", "enqueue", "--session", &continuing]);
}

#[test]
fn a_session_that_is_still_open_is_not_continued() {
    let kestrel = declared();
    let open = opened(&kestrel);

    let refusal = refused(
        &kestrel,
        &[
            "session",
            "open",
            "--organization",
            "acme",
            "--workspace",
            "kestrel",
            "--agent",
            "builder",
            "--continues",
            &open,
        ],
    );

    assert!(
        refusal.contains("continues in it rather than after it"),
        "unhelpful refusal: {refusal}"
    );
}

#[test]
fn a_declaration_is_reachable_only_from_the_organization_it_belongs_to() {
    let kestrel = declared();
    kestrel.run(&["organization", "declare", "globex"]);
    kestrel.run(&[
        "workspace",
        "declare",
        "kestrel",
        "--organization",
        "globex",
        "--repository",
        "https://github.com/globex/kestrel",
        "--branch",
        "trunk",
    ]);

    let workspaces = kestrel.run(&["workspace", "list", "--organization", "globex"]);
    assert_eq!(workspaces.lines().count(), 1);
    assert!(
        workspaces.contains("trunk") && !workspaces.contains("jtmthf"),
        "globex can see acme's workspace: {workspaces}"
    );
    assert_eq!(
        kestrel.run(&["agent", "list", "--organization", "globex"]),
        ""
    );

    let refusal = kestrel.try_run(&[
        "session",
        "open",
        "--organization",
        "globex",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);

    assert!(!refusal.status.success());
    assert!(
        String::from_utf8_lossy(&refusal.stderr)
            .contains("no agent named builder in the organization globex"),
        "acme's agent was reachable from globex: {}",
        String::from_utf8_lossy(&refusal.stderr)
    );
}

#[test]
fn a_run_enqueued_through_the_cli_is_dispatched_and_lists_where_it_executed() {
    let kestrel = declared();
    let session = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);
    let run = kestrel.run(&["run", "enqueue", "--session", &session]);

    assert_eq!(
        kestrel.run(&["run", "list", "--session", &session]),
        format!("{run}  -  queued")
    );

    let listed = dispatched(&kestrel, &session);

    let mut listed = listed.split("  ");
    assert_eq!(listed.next(), Some(run.as_str()));
    assert!(
        listed
            .next()
            .is_some_and(|environment| environment.starts_with("local-exec/")),
        "the run does not list the environment it executed in"
    );
    assert_eq!(listed.next(), Some("succeeded"));
}

#[test]
fn a_dispatched_run_starts_and_ends_in_its_sessions_transcript() {
    let kestrel = declared();
    let session = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);
    let run = kestrel.run(&["run", "enqueue", "--session", &session]);
    dispatched(&kestrel, &session);

    let transcript = kestrel.run(&["session", "transcript", &session]);
    let said: Vec<&str> = transcript.lines().collect();

    assert_eq!(said.len(), 5, "unexpected transcript:\n{transcript}");
    assert!(said[1].ends_with(&format!("run started  {run}")));
    assert!(said[2].ends_with("said  builder  half of one message, and the other half"));
    assert!(said[3].ends_with("said  builder  a second message"));
    assert!(said[4].ends_with(&format!("run ended  {run}  succeeded")));
}

#[test]
fn the_cli_reads_a_transcript_one_window_at_a_time_and_pages_with_the_cursor() {
    let kestrel = declared();
    let session = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);
    kestrel.run(&["run", "enqueue", "--session", &session]);
    dispatched(&kestrel, &session);
    let whole = kestrel.run(&["session", "transcript", &session]);

    let mut walked: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut args = vec!["session", "transcript", session.as_str(), "--window", "2"];
        if let Some(held) = cursor.as_deref() {
            args.extend(["--cursor", held]);
        }

        let read = kestrel.try_run(&args);
        assert!(read.status.success());
        let page = String::from_utf8_lossy(&read.stdout);
        let page = page.trim_end();
        cursor = String::from_utf8_lossy(&read.stderr)
            .lines()
            .find_map(|line| line.strip_prefix("cursor  "))
            .map(str::to_owned);

        if page.is_empty() {
            break;
        }
        assert!(
            page.lines().count() <= 2,
            "a read overran its window:\n{page}"
        );
        walked.extend(page.lines().map(str::to_owned));
    }

    assert_eq!(walked.join("\n"), whole);
    assert_eq!(walked.len(), 5, "unexpected transcript:\n{whole}");
}

/// An Agent says whatever it says, and the CLI hands back a cursor a reader gives straight
/// back, so the two may never be read off the same place.
#[test]
fn what_an_entry_says_cannot_be_mistaken_for_the_cursor() {
    let kestrel = declared();
    let session = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);

    let read = kestrel.try_run(&["session", "transcript", &session]);

    assert!(
        !String::from_utf8_lossy(&read.stdout).contains("cursor  "),
        "the cursor is in the transcript an agent writes into"
    );
    assert!(
        String::from_utf8_lossy(&read.stderr).contains("cursor  "),
        "the read hands back no cursor to resume from"
    );
}

#[test]
fn the_cli_refuses_a_cursor_it_did_not_issue() {
    let kestrel = declared();
    let session = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);

    let nonsense = kestrel.try_run(&[
        "session",
        "transcript",
        &session,
        "--cursor",
        "halfway-through",
    ]);
    assert!(!nonsense.status.success());

    let nowhere = format!("{session}:99");
    let refusal = kestrel.try_run(&["session", "transcript", &session, "--cursor", &nowhere]);
    assert!(!refusal.status.success());
    assert!(
        String::from_utf8_lossy(&refusal.stderr).contains("no position in this transcript"),
        "the read started over rather than refusing: {}",
        String::from_utf8_lossy(&refusal.stderr)
    );
}

#[test]
fn the_cli_refuses_a_window_wider_than_one_read_may_return() {
    let kestrel = declared();
    let session = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);

    let refusal = kestrel.try_run(&["session", "transcript", &session, "--window", "5000"]);

    assert!(!refusal.status.success());
    assert!(
        String::from_utf8_lossy(&refusal.stderr).contains("a window is 1 to 500 entries"),
        "an unbounded read was served: {}",
        String::from_utf8_lossy(&refusal.stderr)
    );
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

/// ADR-0002's definition of done for rung 0.1, out of process and against a real `SIGKILL`:
/// nothing kestrel held in memory lands, and the Environment it provisioned outlives it.
#[test]
fn a_control_plane_killed_mid_run_comes_back_and_the_run_completes() {
    let kestrel = declared();
    let session = kestrel.run(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ]);
    let listen = a_free_port();
    let run = kestrel.run(&["run", "enqueue", "--session", &session]);

    let mut killed = kestrel.booting(&listen, Script::Lingers);
    kestrel.until(
        &["run", "list", "--session", &session],
        |listed| listed.contains("local-exec/"),
        "reached an environment",
    );
    killed.kill().expect("kestrel should be killable");
    killed.wait().expect("kestrel should be waitable");

    let mut restarted = kestrel.booting(&listen, Script::Lingers);
    let listed = kestrel.until(
        &["run", "list", "--session", &session],
        |listed| listed.contains("succeeded") || listed.contains("failed"),
        "ended",
    );
    let _ = restarted.kill();
    restarted.wait().expect("kestrel should be waitable");

    assert!(
        listed.contains("succeeded"),
        "the run did not complete after the restart:\n{listed}"
    );
    assert_eq!(
        transcribed(&kestrel.run(&["session", "transcript", &session])),
        vec![
            "1  participant joined  builder".to_owned(),
            format!("2  run started  {run}"),
            "3  said  builder  half of one message, and the other half".to_owned(),
            "4  said  builder  a second message".to_owned(),
            format!("5  run ended  {run}  succeeded"),
        ]
    );
}

/// The seq and the entry, without the moment it was appended, which is different every run.
fn transcribed(transcript: &str) -> Vec<String> {
    transcript
        .lines()
        .map(|entry| {
            let (seq, rest) = entry.split_once("  ").expect("a seq and an entry");
            let (_, entry) = rest.split_once("  ").expect("an appended-at and an entry");
            format!("{seq}  {entry}")
        })
        .collect()
}
