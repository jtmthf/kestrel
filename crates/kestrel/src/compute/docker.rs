//! The Docker driver: an Environment as a container the daemon on this machine runs (ADR-0005).

use std::io::{self, Write as _};
use std::process::{Child, Command, Stdio};

use super::{Environment, Exited, Provisioned, Streaming};
use crate::domain::RunId;

/// Where the image puts a Workspace, and so what every path an operation takes is relative to.
const WORKSPACE: &str = "/workspace";

#[derive(Debug, Clone)]
pub struct Docker {
    image: String,
}

impl Docker {
    pub fn provisioning_from(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
        }
    }

    pub(super) fn provision(
        &self,
        run: RunId,
        variables: &[(&str, &str)],
    ) -> io::Result<Environment> {
        let container = format!("kestrel-{run}");
        let mut created = vec![
            "create".to_owned(),
            "--name".to_owned(),
            container.clone(),
            // The Environment dials out and nothing dials in (ADR-0002), so this is the only
            // name the link is reachable by from inside.
            "--add-host".to_owned(),
            "host.docker.internal:host-gateway".to_owned(),
        ];
        for (key, value) in variables {
            created.push("--env".to_owned());
            created.push(format!("{key}={value}"));
        }
        created.push(self.image.clone());
        docker(&created.iter().map(String::as_str).collect::<Vec<_>>())?;

        // Started rather than attached, so that by the time this returns the container is
        // running and an operation on it cannot race the daemon into existence.
        if let Err(error) = docker(&["start", &container]) {
            let _ = removed(&container);
            return Err(error);
        }

        let mut logs = match Command::new("docker")
            .args(["logs", "--follow", &container])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(logs) => logs,
            Err(error) => {
                let _ = removed(&container);
                return Err(error);
            }
        };

        Ok(Environment {
            name: format!("docker/{container}"),
            stdout: logs.stdout.take(),
            stderr: logs.stderr.take(),
            provisioned: Box::new(Container { container, logs }),
            destroyed: false,
        })
    }
}

struct Container {
    container: String,
    logs: Child,
}

impl Provisioned for Container {
    fn exec(&mut self, command: &[&str]) -> io::Result<Streaming> {
        let mut arguments = vec!["exec", "--workdir", WORKSPACE, &self.container];
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
        docker(&["exec", "--workdir", WORKSPACE, &self.container, "cat", path])
    }

    fn write_file(&mut self, path: &str, contents: &[u8]) -> io::Result<()> {
        let mut writing = Command::new("docker")
            .args([
                "exec",
                "--interactive",
                "--workdir",
                WORKSPACE,
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
            "{path} could not be written in the environment: {}",
            String::from_utf8_lossy(&written.stderr).trim()
        )))
    }

    /// Asked of the daemon rather than of a client attached to it, so a container that died
    /// while nothing was watching is still found to be gone.
    fn status(&mut self) -> io::Result<Option<Exited>> {
        let inspected = match docker(&[
            "inspect",
            "--format",
            "{{.State.Running}} {{.State.ExitCode}}",
            &self.container,
        ]) {
            Ok(inspected) => inspected,
            Err(error) if gone(&error) => return Ok(Some(Exited::without_a_code())),
            Err(error) => return Err(error),
        };

        let inspected = String::from_utf8_lossy(&inspected);
        let (running, code) = inspected
            .trim()
            .split_once(' ')
            .ok_or_else(|| io::Error::other(format!("docker inspect said {inspected:?}")))?;

        if running == "true" {
            return Ok(None);
        }

        Ok(Some(match code.parse() {
            Ok(code) => Exited::with(code),
            Err(_) => Exited::without_a_code(),
        }))
    }

    fn destroy(&mut self) -> io::Result<()> {
        let _ = self.logs.kill();
        let _ = self.logs.wait();

        removed(&self.container)
    }
}

fn removed(container: &str) -> io::Result<()> {
    match docker(&["rm", "--force", "--volumes", container]) {
        Err(error) if gone(&error) => Ok(()),
        removed => removed.map(drop),
    }
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

fn gone(error: &io::Error) -> bool {
    error.to_string().contains("No such container")
}
