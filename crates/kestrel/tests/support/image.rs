//! The `kestrel-env` image as a test drives it: built rather than assumed present, and run
//! the way an operator running one by hand would run it.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use kestrel::domain::RunId;
use kestrel::link::credential::Secret;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

const IMAGE: &str = "kestrel-env:test";
const PATIENCE: Duration = Duration::from_secs(30);

pub fn built() -> &'static str {
    static BUILT: OnceLock<()> = OnceLock::new();

    BUILT.get_or_init(|| {
        let built = Command::new("docker")
            .current_dir(repository())
            .args([
                "build",
                "--file",
                "images/kestrel-env/Dockerfile",
                "--tag",
                IMAGE,
                ".",
            ])
            .output()
            .expect("docker should build the image");

        assert!(
            built.status.success(),
            "building {IMAGE} failed:\n{}",
            String::from_utf8_lossy(&built.stderr)
        );
    });

    IMAGE
}

#[derive(Debug)]
pub struct Ran {
    pub code: i32,
    pub out: String,
    pub err: String,
}

/// The given command run inside the image instead of the supervisor.
pub fn running(command: &[&str]) -> Ran {
    let mut arguments = vec!["run", "--rm", "--entrypoint", command[0], built()];
    arguments.extend_from_slice(&command[1..]);

    let ran = Command::new("docker")
        .args(&arguments)
        .output()
        .expect("docker should run the image");

    Ran {
        code: ran.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&ran.stdout).trim().to_owned(),
        err: String::from_utf8_lossy(&ran.stderr).trim().to_owned(),
    }
}

pub fn configured(field: &str) -> String {
    let inspected = Command::new("docker")
        .args(["image", "inspect", "--format", field, built()])
        .output()
        .expect("docker should inspect the image");

    assert!(
        inspected.status.success(),
        "inspecting {IMAGE} failed:\n{}",
        String::from_utf8_lossy(&inspected.stderr)
    );

    String::from_utf8_lossy(&inspected.stdout).trim().to_owned()
}

pub struct Container {
    name: String,
    running: Child,
    diagnostics: UnboundedReceiver<String>,
    seen: Vec<String>,
    removed: bool,
}

impl Container {
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
        let (lines, diagnostics) = unbounded_channel();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });

        Self {
            name,
            running,
            diagnostics,
            seen: Vec::new(),
            removed: false,
        }
    }

    pub async fn wait_until_it_says(&mut self, what: &str) {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        loop {
            if self.seen.iter().any(|line| line.contains(what)) {
                return;
            }
            match tokio::time::timeout_at(deadline, self.diagnostics.recv()).await {
                Ok(Some(line)) => self.seen.push(line),
                Ok(None) => panic!(
                    "the container stopped saying anything before it said {what:?}. it said:\n{}",
                    self.everything_it_said()
                ),
                Err(_) => panic!(
                    "timed out waiting for the container to say {what:?}. it said:\n{}",
                    self.everything_it_said()
                ),
            }
        }
    }

    /// A signal sent from outside the container, because the kernel refuses one sent to pid 1
    /// from a process sharing its namespace — which is what `docker exec` would be.
    pub fn kill_the_supervisor(&self) {
        let killed = Command::new("docker")
            .args(["kill", "--signal", "KILL", &self.name])
            .output()
            .expect("docker should kill the container");

        assert!(
            killed.status.success(),
            "killing the supervisor in {} failed:\n{}",
            self.name,
            String::from_utf8_lossy(&killed.stderr)
        );
    }

    pub async fn exits(&mut self) -> i32 {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        loop {
            if let Some(status) = self
                .running
                .try_wait()
                .expect("the container should be waitable")
            {
                while let Ok(line) = self.diagnostics.try_recv() {
                    self.seen.push(line);
                }
                return status.code().unwrap_or(-1);
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the container is still running after {PATIENCE:?}. it said:\n{}",
                self.everything_it_said()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn state(&self) -> String {
        self.inspect("{{.State.Status}}")
    }

    pub fn restarts(&self) -> u32 {
        let restarts = self.inspect("{{.RestartCount}}");

        restarts
            .parse()
            .unwrap_or_else(|_| panic!("docker gave {restarts:?} as a restart count"))
    }

    fn inspect(&self, field: &str) -> String {
        let inspected = Command::new("docker")
            .args(["inspect", "--format", field, &self.name])
            .output()
            .expect("docker should inspect the container");

        assert!(
            inspected.status.success(),
            "inspecting {} failed:\n{}",
            self.name,
            String::from_utf8_lossy(&inspected.stderr)
        );

        String::from_utf8_lossy(&inspected.stdout).trim().to_owned()
    }

    pub fn everything_it_said(&self) -> String {
        self.seen.join("\n")
    }

    pub fn destroy(mut self) {
        remove(&self.name);
        self.removed = true;
    }
}

impl Drop for Container {
    /// A test that panics before calling `destroy` must not leave a container behind either.
    fn drop(&mut self) {
        if !self.removed {
            remove(&self.name);
        }
        let _ = self.running.wait();
    }
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
