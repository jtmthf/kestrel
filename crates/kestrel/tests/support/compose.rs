//! The shipped compose stack as a test drives it: built and brought up the way the README
//! says to, driven through the CLI role inside it, and torn down with its volume.

use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use super::docker::{Ran, repository};

pub const CONTROL_PLANE: &str = "kestrel";
pub const FILTER: &str = "socket-proxy";
const ENVIRONMENT: &str = "kestrel-env";
const LINK: &str = "kestrel-link";
const PATIENCE: Duration = Duration::from_secs(60);

/// One project name, one host daemon, one volume: two stacks at once would fight over all
/// three.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

pub struct Stack {
    _one_at_a_time: MutexGuard<'static, ()>,
}

impl Stack {
    pub fn up() -> Self {
        let stack = Self {
            _one_at_a_time: ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner),
        };
        built();
        stack.down_with_its_volume();
        stack.start();

        stack
    }

    /// Down and up again the way an operator restarts one: the volume the database is on is
    /// what `down` leaves behind.
    pub fn comes_back(&self) {
        completed(&["down"], "bringing the stack down");
        self.start();
    }

    /// The CLI role in the control plane, which does its one thing and exits.
    pub fn ran(&self, command: &[&str]) -> String {
        let mut kestrel = vec!["kestrel"];
        kestrel.extend_from_slice(command);
        let ran = self.in_the_control_plane(&kestrel);
        assert_eq!(
            ran.code,
            0,
            "`kestrel {}` in the stack failed:\n{}",
            command.join(" "),
            ran.err
        );

        ran.out
    }

    pub fn in_the_control_plane(&self, command: &[&str]) -> Ran {
        let mut exec = vec!["exec", "--no-TTY", CONTROL_PLANE];
        exec.extend_from_slice(command);

        ran(&exec)
    }

    /// A throwaway container where an Environment would be: the same image, on the same
    /// network, reaching for whatever a test hands it.
    pub fn on_the_link_an_environment_dials(&self, command: &[&str]) -> Ran {
        let (program, arguments) = command.split_first().expect("a command to run");
        let mut run = vec![
            "run",
            "--rm",
            "--network",
            LINK,
            "--entrypoint",
            program,
            ENVIRONMENT,
        ];
        run.extend_from_slice(arguments);

        super::docker::ran(&run)
    }

    pub fn everything_a_service_said(&self, service: &str) -> String {
        let said = ran(&["logs", "--no-color", service]);

        format!("{}\n{}", said.out, said.err)
    }

    fn start(&self) {
        completed(&["up", "--detach", "--wait"], "bringing the stack up");
    }

    fn down_with_its_volume(&self) {
        completed(
            &["down", "--volumes", "--remove-orphans"],
            "bringing the stack down",
        );
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.down_with_its_volume();
    }
}

/// Every image the compose file names, built once for every test in this binary.
pub fn built() -> &'static [String] {
    static BUILT: OnceLock<Vec<String>> = OnceLock::new();

    BUILT.get_or_init(|| {
        completed(&["build"], "building the images the compose file names");

        completed(&["config", "--images"], "listing the images")
            .lines()
            .map(str::to_owned)
            .collect()
    })
}

/// The compose file rendered with nothing in the environment but a path to docker and the
/// context it reads: what an operator has to supply shows up here as a warning.
pub fn rendered_against_an_empty_environment() -> Ran {
    let mut rendering = Command::new("docker");
    rendering.current_dir(repository()).env_clear();
    for kept in ["PATH", "HOME"] {
        if let Ok(value) = std::env::var(kept) {
            rendering.env(kept, value);
        }
    }

    let rendered = rendering
        .args(["compose", "config"])
        .output()
        .expect("docker should be reachable");

    Ran {
        code: rendered.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&rendered.stdout).trim().to_owned(),
        err: String::from_utf8_lossy(&rendered.stderr).trim().to_owned(),
    }
}

pub fn until<T>(what: &str, ready: impl Fn() -> Option<T>) -> T {
    let deadline = Instant::now() + PATIENCE;

    loop {
        if let Some(ready) = ready() {
            return ready;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn completed(arguments: &[&str], doing: &str) -> String {
    let ran = ran(arguments);
    assert_eq!(ran.code, 0, "{doing} failed:\n{}", ran.err);

    ran.out
}

fn ran(arguments: &[&str]) -> Ran {
    let mut compose = vec!["compose"];
    compose.extend_from_slice(arguments);

    super::docker::ran(&compose)
}
