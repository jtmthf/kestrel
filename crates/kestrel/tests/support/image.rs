//! The `kestrel-env` image as a test drives it: the one CI built for this change, or one built
//! here when nothing named it, and run the way an operator running one by hand would run it.

use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use kestrel::domain::RunId;
use kestrel::link::credential::Secret;

use super::diagnostics::Diagnostics;
use super::docker::{self, Ran, removed};
use super::images;

const LOCAL: &str = "kestrel-env:test";
const SCRIPTED: &str = "kestrel-env-scripted:test";
const CONFORMANCE: &str = "kestrel-env-conformance:test";
const DEVELOPMENT: &str = "kestrel-dev:test";
const PATIENCE: Duration = Duration::from_secs(30);

/// The image a test provisions from: what CI built for this change and named, or one built here
/// for local runs.
pub fn built() -> &'static str {
    static BUILT: OnceLock<String> = OnceLock::new();

    BUILT.get_or_init(|| match images::sourced(images::ENV) {
        Some(image) => image,
        None => {
            docker::completed(
                &[
                    "build",
                    "--file",
                    "images/kestrel-env/Dockerfile",
                    "--tag",
                    LOCAL,
                    ".",
                ],
                "building the image",
            );
            LOCAL.to_owned()
        }
    })
}

/// The image with the scripted ACP agent in it, which is the only thing an Environment needs
/// that the shipped image has no business carrying.
pub fn with_the_scripted_agent() -> &'static str {
    static BUILT: OnceLock<()> = OnceLock::new();

    BUILT.get_or_init(|| {
        built();
        docker::completed(
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

/// The image with the adapter the conformance suite's second agent is reached through, which
/// the shipped image has no business carrying (ADR-0007).
pub fn with_the_adapter() -> &'static str {
    static BUILT: OnceLock<()> = OnceLock::new();

    BUILT.get_or_init(|| {
        built();
        docker::completed(
            &[
                "build",
                "--file",
                "crates/kestrel/tests/support/conformance-env.Dockerfile",
                "--tag",
                CONFORMANCE,
                ".",
            ],
            "building the image with the adapter",
        );
    });

    CONFORMANCE
}

/// The development image, derived from the `kestrel-env` this suite builds rather than from
/// whatever an operator last tagged `kestrel-env`.
pub fn development() -> &'static str {
    static BUILT: OnceLock<()> = OnceLock::new();

    BUILT.get_or_init(|| {
        let base = format!("KESTREL_ENV={}", built());
        docker::completed(
            &[
                "build",
                "--file",
                "images/kestrel-dev/Dockerfile",
                "--build-arg",
                &base,
                "--tag",
                DEVELOPMENT,
                ".",
            ],
            "building the development image",
        );
    });

    DEVELOPMENT
}

/// The container behind an Instance a Run recorded.
pub struct Container(String);

impl Container {
    pub fn named(instance: &str) -> Self {
        let (driver, container) = instance
            .split_once('/')
            .unwrap_or_else(|| panic!("{instance} does not name a driver and an instance"));
        assert_eq!(driver, "docker");

        Self(container.to_owned())
    }

    /// Every command line running in the container. The image carries no `ps`, so they are
    /// read where the kernel keeps them.
    pub fn processes(&self) -> String {
        self.exec(&[
            "sh",
            "-c",
            r#"for p in /proc/[0-9]*; do tr '\0' ' ' < "$p/cmdline" 2>/dev/null; echo; done"#,
        ])
        .out
    }

    /// An Instance outlives its Session's Runs, so a test that provisions one removes it.
    pub fn destroy(&self) {
        docker::ran(&["rm", "--force", "--volumes", &self.0]);
    }

    pub fn exec(&self, command: &[&str]) -> Ran {
        let mut arguments = vec!["exec", &self.0];
        arguments.extend_from_slice(command);

        docker::ran(&arguments)
    }

    pub fn everything_it_said(&self) -> String {
        let said = docker::ran(&["logs", &self.0]);

        format!("{}\n{}", said.out, said.err)
    }

    pub fn networks(&self) -> String {
        docker::completed(
            &[
                "inspect",
                "--format",
                "{{range $network, $conf := .NetworkSettings.Networks}}{{$network}} {{end}}",
                &self.0,
            ],
            "inspecting the environment's networks",
        )
    }

    pub fn image(&self) -> String {
        docker::completed(
            &["inspect", "--format", "{{.Config.Image}}", &self.0],
            "inspecting the environment's image",
        )
    }

    pub fn kill(&self) {
        docker::completed(
            &["kill", "--signal", "KILL", &self.0],
            "killing a container",
        );
    }

    pub async fn is_gone(&self) {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        while tokio::time::Instant::now() < deadline {
            if docker::ran(&["inspect", "--format", "{{.Id}}", &self.0]).code != 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        panic!("the container {} was never destroyed", self.0);
    }
}

/// Run instead of the supervisor the image would otherwise start.
pub fn running(command: &[&str]) -> Ran {
    docker::running(built(), command)
}

pub fn configured(field: &str) -> String {
    docker::configured(built(), field)
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
        removed(&name);

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
        docker::completed(
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
        docker::completed(
            &["inspect", "--format", "{{.State.Status}}", &self.name],
            "inspecting the environment",
        )
    }

    pub fn destroy(mut self) {
        removed(&self.name);
        self.destroyed = true;
    }
}

impl Drop for Environment {
    /// A test that panics before calling `destroy` must not leave a container behind either.
    fn drop(&mut self) {
        if !self.destroyed {
            removed(&self.name);
        }
        let _ = self.running.wait();
    }
}
