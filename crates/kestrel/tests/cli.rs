use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

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

    /// `Child::kill` is a `SIGKILL`, so nothing kestrel holds in memory is given a chance to land.
    fn kill_a_booted_control_plane(&self) {
        let mut kestrel = Command::new(env!("CARGO_BIN_EXE_kestrel"))
            .env("KESTREL_DATA_DIR", self.data_dir.path())
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("kestrel should spawn");

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
