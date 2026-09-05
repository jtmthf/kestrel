//! The `kestrel-env` image as a test drives it: built rather than assumed present, and run
//! the way an operator running one by hand would run it.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use kestrel::domain::RunId;
use kestrel::link::credential::Secret;

use super::diagnostics::Diagnostics;

const IMAGE: &str = "kestrel-env:test";
const SCRIPTED: &str = "kestrel-env-scripted:test";
const PATIENCE: Duration = Duration::from_secs(30);

pub fn built() -> &'static str {
    static BUILT: OnceLock<()> = OnceLock::new();

    BUILT.get_or_init(|| {
        docker(
            &[
                "build",
                "--file",
                "images/kestrel-env/Dockerfile",
                "--tag",
                IMAGE,
                ".",
            ],
            "building the image",
        );
    });

    IMAGE
}

/// The image with the scripted ACP agent in it, which is the only thing an Environment needs
/// that the shipped image has no business carrying.
pub fn with_the_scripted_agent() -> &'static str {
    static BUILT: OnceLock<()> = OnceLock::new();

    BUILT.get_or_init(|| {
        built();
        docker(
            &[
                "build",
                "--file",
                "crates/kestrel/tests/support/scripted-env.Dockerfile",
                "--tag",
                SCRIPTED,
                ".",
            ],
            "building the image with the scripted agent",
        );
    });

    SCRIPTED
}

/// The container behind an Environment a Run recorded.
pub struct Container(String);

impl Container {
    pub fn named(environment: &str) -> Self {
        let (driver, container) = environment
            .split_once('/')
            .unwrap_or_else(|| panic!("{environment} does not name a driver and an instance"));
        assert_eq!(driver, "docker");

        Self(container.to_owned())
    }

    pub fn exec(&self, command: &[&str]) -> Ran {
        let mut arguments = vec!["exec", &self.0];
        arguments.extend_from_slice(command);

        ran(&arguments)
    }

    pub fn kill(&self) {
        docker(
            &["kill", "--signal", "KILL", &self.0],
            "killing a container",
        );
    }

    pub async fn is_gone(&self) {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        while tokio::time::Instant::now() < deadline {
            if ran(&["inspect", "--format", "{{.Id}}", &self.0]).code != 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        panic!("the container {} was never destroyed", self.0);
    }
}

#[derive(Debug)]
pub struct Ran {
    pub code: i32,
    pub out: String,
    pub err: String,
}

/// Run instead of the supervisor the image would otherwise start.
pub fn running(command: &[&str]) -> Ran {
    let (program, arguments) = command.split_first().expect("a command to run");
    let mut run = vec!["run", "--rm", "--entrypoint", program, built()];
    run.extend_from_slice(arguments);

    ran(&run)
}

fn ran(arguments: &[&str]) -> Ran {
    let ran = Command::new("docker")
        .args(arguments)
        .output()
        .expect("docker should be reachable");

    Ran {
        code: ran.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&ran.stdout).trim().to_owned(),
        err: String::from_utf8_lossy(&ran.stderr).trim().to_owned(),
    }
}

pub fn configured(field: &str) -> String {
    docker(
        &["image", "inspect", "--format", field, built()],
        "inspecting the image",
    )
}

pub struct Environment {
    name: String,
    running: Child,
    diagnostics: Diagnostics,
    destroyed: bool,
}

impl Environment {
    pub fn provision(link: &str, run: RunId, credential: &Secret) -> Self {
        let image = built();
        let name = format!("kestrel-env-{run}");
        remove(&name);

        let mut running = Command::new("docker")
            .args([
                "run",
                "--name",
                &name,
                "--add-host",
                "host.docker.internal:host-gateway",
                "--env",
                &format!("KESTREL_LINK={link}"),
                "--env",
                &format!("KESTREL_RUN={run}"),
                "--env",
                &format!("KESTREL_RUN_CREDENTIAL={}", credential.as_str()),
                "--env",
                "KESTREL_AGENT_RUNTIME=opencode acp",
                image,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("docker should run the image");

        let pipe = running
            .stderr
            .take()
            .expect("the supervisor's diagnostics should be piped");

        Self {
            name,
            running,
            diagnostics: Diagnostics::pumped("the environment", pipe),
            destroyed: false,
        }
    }

    pub async fn wait_until_it_says(&mut self, what: &str) {
        self.diagnostics.wait_until_it_says(what).await;
    }

    pub fn everything_it_said(&self) -> String {
        self.diagnostics.everything_it_said()
    }

    /// A signal sent from outside, because the kernel refuses one sent to pid 1 from a process
    /// sharing its namespace — which is what `docker exec` would be.
    pub fn kill_the_supervisor(&self) {
        docker(
            &["kill", "--signal", "KILL", &self.name],
            "killing the supervisor",
        );
    }

    pub async fn exits(&mut self) -> i32 {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        loop {
            if let Some(status) = self
                .running
                .try_wait()
                .expect("the environment should be waitable")
            {
                self.diagnostics.drain();
                return status.code().unwrap_or(-1);
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the environment is still running after {PATIENCE:?}. it said:\n{}",
                self.everything_it_said()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn state(&self) -> String {
        docker(
            &["inspect", "--format", "{{.State.Status}}", &self.name],
            "inspecting the environment",
        )
    }

    pub fn destroy(mut self) {
        remove(&self.name);
        self.destroyed = true;
    }
}

impl Drop for Environment {
    /// A test that panics before calling `destroy` must not leave a container behind either.
    fn drop(&mut self) {
        if !self.destroyed {
            remove(&self.name);
        }
        let _ = self.running.wait();
    }
}

fn docker(arguments: &[&str], doing: &str) -> String {
    let ran = Command::new("docker")
        .current_dir(repository())
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("docker should be reachable for {doing}: {error}"));

    assert!(
        ran.status.success(),
        "{doing} failed:\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );

    String::from_utf8_lossy(&ran.stdout).trim().to_owned()
}

fn remove(name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "--force", "--volumes", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two directories under the repository")
        .to_path_buf()
}
