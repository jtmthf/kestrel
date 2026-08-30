//! The same artifact `kestrel-env` ships, in a local-exec Environment, dialling out over the
//! same link an operator would.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::OnceLock;
use std::time::Duration;

use kestrel::compute::{Environment, LocalExec};
use kestrel::domain::RunId;
use kestrel::link::credential::Secret;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

const PATIENCE: Duration = Duration::from_secs(30);

/// Built rather than assumed present, so a Rust test never passes against a stale artifact
/// someone built by hand.
pub fn binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();

    BINARY.get_or_init(|| {
        let alongside = alongside_this_test();
        let profile = alongside
            .file_name()
            .and_then(|profile| profile.to_str())
            .expect("a named profile directory");
        let built = Command::new(env!("CARGO"))
            .args([
                "build",
                "--package",
                "kestrel-supervisor",
                "--profile",
                if profile == "debug" { "dev" } else { profile },
            ])
            .output()
            .expect("cargo should build the supervisor");

        assert!(
            built.status.success(),
            "building kestrel-supervisor failed:\n{}",
            String::from_utf8_lossy(&built.stderr)
        );

        let binary = alongside.join("kestrel-supervisor");
        assert!(
            binary.exists(),
            "cargo built the supervisor, but not to {}",
            binary.display()
        );
        binary
    })
}

fn alongside_this_test() -> PathBuf {
    std::env::current_exe()
        .expect("a test binary should know where it is")
        .parent()
        .and_then(Path::parent)
        .expect("the test binary sits in the profile's deps directory")
        .to_path_buf()
}

pub struct Supervisor {
    environment: Environment,
    diagnostics: UnboundedReceiver<String>,
    seen: Vec<String>,
}

impl Supervisor {
    pub fn provision(link: &str, run: RunId, credential: &Secret) -> Self {
        let run = run.to_string();
        let mut environment = LocalExec
            .provision(
                binary(),
                &[],
                &[
                    ("KESTREL_LINK", link),
                    ("KESTREL_RUN", &run),
                    ("KESTREL_RUN_CREDENTIAL", credential.as_str()),
                ],
            )
            .expect("the supervisor should spawn");

        let pipe = environment
            .take_stderr()
            .expect("the supervisor's diagnostics should be piped");
        let (lines, diagnostics) = unbounded_channel();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });

        Self {
            environment,
            diagnostics,
            seen: Vec::new(),
        }
    }

    pub async fn wait_until_it_says(&mut self, what: &str) {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        loop {
            if self.said(what) {
                return;
            }
            match tokio::time::timeout_at(deadline, self.diagnostics.recv()).await {
                Ok(Some(line)) => self.seen.push(line),
                Ok(None) => panic!(
                    "the supervisor stopped saying anything before it said {what:?}. it said:\n{}",
                    self.everything_it_said()
                ),
                Err(_) => panic!(
                    "timed out waiting for the supervisor to say {what:?}. it said:\n{}",
                    self.everything_it_said()
                ),
            }
        }
    }

    pub async fn exits(&mut self) -> ExitStatus {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        loop {
            if let Some(status) = self
                .environment
                .status()
                .expect("the supervisor should be waitable")
            {
                self.drain();
                return status;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "the supervisor is still running after {PATIENCE:?}. it said:\n{}",
                    self.everything_it_said()
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn said(&self, what: &str) -> bool {
        self.seen.iter().any(|line| line.contains(what))
    }

    pub fn everything_it_said(&self) -> String {
        self.seen.join("\n")
    }

    fn drain(&mut self) {
        while let Ok(line) = self.diagnostics.try_recv() {
            self.seen.push(line);
        }
    }

    pub fn destroy(self) {
        LocalExec
            .destroy(self.environment)
            .expect("the environment should be destroyed");
    }
}

impl Supervisor {
    pub async fn is_still_running(&mut self, after: Duration) -> bool {
        tokio::time::sleep(after).await;

        self.environment
            .status()
            .expect("the supervisor should be waitable")
            .is_none()
    }
}
