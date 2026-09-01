//! The same artifact `kestrel-env` ships, in a local-exec Environment, dialling out over the
//! same link an operator would.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::OnceLock;
use std::time::Duration;

use kestrel::compute::{Environment, LocalExec};
use kestrel::domain::RunId;
use kestrel::link::credential::Secret;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::{built, scripted_agent};

const PATIENCE: Duration = Duration::from_secs(30);

pub fn binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();

    BINARY.get_or_init(|| built::binary("kestrel-supervisor"))
}

pub struct Supervisor {
    environment: Environment,
    diagnostics: UnboundedReceiver<String>,
    seen: Vec<String>,
}

impl Supervisor {
    pub fn provision(link: &str, run: RunId, credential: &Secret) -> Self {
        let run = run.to_string();
        let runtime = scripted_agent::playing(scripted_agent::Script::Speaks);
        let mut environment = LocalExec
            .provision(
                binary(),
                &[],
                &[
                    ("KESTREL_LINK", link),
                    ("KESTREL_RUN", &run),
                    ("KESTREL_RUN_CREDENTIAL", credential.as_str()),
                    ("KESTREL_AGENT_RUNTIME", &runtime),
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

    /// Signals nothing: a supervisor that reported itself finished is on its way out, and
    /// reaping it is the whole of the cleanup left.
    pub async fn finishes(mut self) -> ExitStatus {
        self.exits().await
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
