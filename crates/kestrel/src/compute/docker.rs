//! The Docker driver: an Instance as a container the daemon on this machine runs (ADR-0005).

use std::io::{self, Write as _};
use std::process::{Child, Command, Stdio};

use super::{Exited, Instance, Provisioned, Streaming, Supervising, Supervisor};
use crate::domain::RunId;

/// Where the image puts an Instance's checkouts, and so what every path an operation takes is
/// relative to.
const ROOT: &str = "/workspace";

#[derive(Debug, Clone)]
pub struct Docker {
    image: String,
    network: Option<String>,
}

impl Docker {
    pub fn provisioning_from(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            network: None,
        }
    }

    /// What a control plane in a container beside the Instance is reached over, where the
    /// host's gateway reaches nothing.
    pub fn on_network(mut self, network: impl Into<String>) -> Self {
        self.network = Some(network.into());
        self
    }

    pub(super) fn provision(&self, run: RunId) -> io::Result<Instance> {
        let container = format!("kestrel-{run}");
        let mut created = vec![
            "create".to_owned(),
            "--name".to_owned(),
            container.clone(),
            // The Instance dials out and nothing dials in (ADR-0002), so this is the only
            // name the link is reachable by from inside.
            "--add-host".to_owned(),
            "host.docker.internal:host-gateway".to_owned(),
        ];
        if let Some(network) = &self.network {
            created.push("--network".to_owned());
            created.push(network.clone());
        }
        // As the first process, the one thing stopping a supervisor leaves running and the
        // one thing no process in the container can signal.
        created.extend(["--entrypoint", "sleep", &self.image, "infinity"].map(str::to_owned));
        docker(&created.iter().map(String::as_str).collect::<Vec<_>>())?;

        // Started rather than attached, so that by the time this returns the container is
        // running and an operation on it cannot race the daemon into existence.
        if let Err(error) = docker(&["start", &container]) {
            let _ = removed(&container);
            return Err(error);
        }

        Ok(in_container(container))
    }

    /// A container that stopped still holds its filesystem, so it is started again rather than
    /// taken for gone.
    pub(super) fn resume(&self, instance: &str) -> io::Result<Option<Instance>> {
        let container = named(instance)?;
        let running = match docker(&["inspect", "--format", "{{.State.Running}}", container]) {
            Ok(running) => running,
            Err(error) if gone(&error) => return Ok(None),
            Err(error) => return Err(error),
        };

        if String::from_utf8_lossy(&running).trim() != "true" {
            docker(&["start", container])?;
        }

        Ok(Some(in_container(container.to_owned())))
    }
}

fn in_container(container: String) -> Instance {
    Instance {
        name: format!("docker/{container}"),
        provisioned: Box::new(Container { container }),
    }
}

struct Container {
    container: String,
}

impl Provisioned for Container {
    fn exec(&mut self, command: &[&str]) -> io::Result<Streaming> {
        let mut arguments = vec!["exec", "--workdir", ROOT, &self.container];
        arguments.extend_from_slice(command);

        let child = Command::new("docker")
            .args(&arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        Ok(Streaming { child })
    }

    fn read_file(&mut self, path: &str) -> io::Result<Vec<u8>> {
        docker(&["exec", "--workdir", ROOT, &self.container, "cat", path])
    }

    fn write_file(&mut self, path: &str, contents: &[u8]) -> io::Result<()> {
        let mut writing = Command::new("docker")
            .args([
                "exec",
                "--interactive",
                "--workdir",
                ROOT,
                &self.container,
                "sh",
                "-c",
                r#"mkdir -p "$(dirname "$1")" && cat > "$1""#,
                "sh",
                path,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        writing
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(contents)?;

        let written = writing.wait_with_output()?;
        if written.status.success() {
            return Ok(());
        }

        Err(io::Error::other(format!(
            "{path} could not be written in the instance: {}",
            String::from_utf8_lossy(&written.stderr).trim()
        )))
    }

    /// Named rather than given on the command line, so a Run's credentials are in no process
    /// listing on this machine and in nothing the container's configuration keeps.
    fn supervise(&mut self, variables: &[(&str, &str)]) -> io::Result<Supervisor> {
        let mut command = Command::new("docker");
        command.args(["exec", "--workdir", ROOT]);
        for (key, value) in variables {
            command.args(["--env", key]).env(key, value);
        }
        let mut exec = command
            .args([&self.container, "kestrel-supervisor"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        Ok(Supervisor {
            name: format!("docker/{}", self.container),
            stdout: exec.stdout.take(),
            stderr: exec.stderr.take(),
            supervising: Box::new(Exec {
                container: self.container.clone(),
                exec,
            }),
            stopped: false,
        })
    }

    fn destroy(&mut self) -> io::Result<()> {
        removed(&self.container)
    }
}

/// The client `docker exec` leaves on this machine, which exits with the supervisor it runs.
struct Exec {
    container: String,
    exec: Child,
}

impl Supervising for Exec {
    fn status(&mut self) -> io::Result<Option<Exited>> {
        Ok(self.exec.try_wait()?.map(Exited::from))
    }

    fn stop(&mut self) -> io::Result<()> {
        let stopped = stopped(&self.container);
        let _ = self.exec.kill();
        let _ = self.exec.wait();

        stopped
    }
}

/// Killing the client leaves what it started running in the container, and an agent is free to
/// start processes that leave the supervisor's group, so everything but the first process goes.
fn stopped(container: &str) -> io::Result<()> {
    match docker(&[
        "exec",
        container,
        "sh",
        "-c",
        "kill -KILL -1 2>/dev/null; true",
    ]) {
        Err(error) if gone(&error) || error.to_string().contains("is not running") => Ok(()),
        stopped => stopped.map(drop),
    }
}

pub(super) fn stop_named(supervisor: &str) -> io::Result<()> {
    stopped(named(supervisor)?)
}

fn removed(container: &str) -> io::Result<()> {
    match docker(&["rm", "--force", "--volumes", container]) {
        Err(error) if gone(&error) => Ok(()),
        removed => removed.map(drop),
    }
}

pub(super) fn destroy_named(instance: &str) -> io::Result<()> {
    removed(named(instance)?)
}

fn named(instance: &str) -> io::Result<&str> {
    instance
        .strip_prefix("docker/")
        .ok_or_else(|| io::Error::other(format!("{instance} is not a Docker instance")))
}

fn docker(arguments: &[&str]) -> io::Result<Vec<u8>> {
    let ran = Command::new("docker")
        .args(arguments)
        .stdin(Stdio::null())
        .output()?;

    if ran.status.success() {
        return Ok(ran.stdout);
    }

    Err(io::Error::other(format!(
        "`docker {}` failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&ran.stderr).trim()
    )))
}

/// `inspect` says "no such object" where every other command says "No such container".
fn gone(error: &io::Error) -> bool {
    let said = error.to_string().to_lowercase();

    said.contains("no such container") || said.contains("no such object")
}
