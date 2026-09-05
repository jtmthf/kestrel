//! The same artifact `kestrel-env` ships, in a local-exec Environment, dialling out over the
//! same link an operator would.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::OnceLock;
use std::time::Duration;

use kestrel::compute::{Environment, LocalExec};
use kestrel::domain::RunId;
use kestrel::link::credential::Secret;

use super::diagnostics::Diagnostics;
use super::{built, scripted_agent};

const PATIENCE: Duration = Duration::from_secs(30);

pub fn binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();

    BINARY.get_or_init(|| built::binary("kestrel-supervisor"))
}

pub struct Supervisor {
    environment: Environment,
    diagnostics: Diagnostics,
}

impl Supervisor {
    pub fn provision(link: &str, run: RunId, credential: &Secret) -> Self {
        Self::provision_playing(link, run, credential, scripted_agent::Script::Speaks)
    }

    pub fn provision_playing(
        link: &str,
        run: RunId,
        credential: &Secret,
        script: scripted_agent::Script,
    ) -> Self {
        let run = run.to_string();
        let runtime = scripted_agent::playing(script);
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

        Self {
            environment,
            diagnostics: Diagnostics::pumped("the supervisor", pipe),
        }
    }

    pub async fn wait_until_it_says(&mut self, what: &str) {
        self.diagnostics.wait_until_it_says(what).await;
    }

    pub async fn exits(&mut self) -> ExitStatus {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        loop {
            if let Some(status) = self
                .environment
                .status()
                .expect("the supervisor should be waitable")
            {
                self.diagnostics.drain();
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
        self.diagnostics.said(what)
    }

    pub fn everything_it_said(&self) -> String {
        self.diagnostics.everything_it_said()
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
