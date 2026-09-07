//! The one port that is driven twice (ADR-0005): the Docker daemon, and a local process tree.

mod docker;
mod local_exec;

use std::fmt;
use std::io;
use std::process::{Child, ChildStderr, ChildStdout, ExitStatus};

pub use docker::Docker;
pub use local_exec::LocalExec;

use crate::domain::RunId;

/// What a driver does once it has provisioned, and the whole of it. Pause, resume, a disk that
/// outlives a Run and an inbound address each split the eight deployment targets, so no driver
/// offers them and none may add them.
pub trait Provisioned: Send {
    fn exec(&mut self, command: &[&str]) -> io::Result<Streaming>;
    fn read_file(&mut self, path: &str) -> io::Result<Vec<u8>>;
    fn write_file(&mut self, path: &str, contents: &[u8]) -> io::Result<()>;
    fn status(&mut self) -> io::Result<Option<Exited>>;
    fn destroy(&mut self) -> io::Result<()>;
}

/// The sixth operation, and the only place either driver is named: which one executes a Run is
/// read from configuration once, never decided where a Run is executed.
#[derive(Debug, Clone)]
pub enum Driver {
    Docker(Docker),
    LocalExec(LocalExec),
}

impl Driver {
    pub fn provision(&self, run: RunId, variables: &[(&str, &str)]) -> io::Result<Environment> {
        match self {
            Driver::Docker(docker) => docker.provision(run, variables),
            Driver::LocalExec(local_exec) => local_exec.provision(run, variables),
        }
    }

    pub fn destroy_named(&self, run: RunId, environment: &str) -> io::Result<()> {
        match self {
            Driver::Docker(_) => docker::destroy_named(environment),
            Driver::LocalExec(_) => local_exec::destroy_named(run, environment),
        }
    }
}

pub struct Environment {
    name: String,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    provisioned: Box<dyn Provisioned>,
    destroyed: bool,
}

impl Environment {
    /// `<driver>/<instance>`, which is what a Run records having executed in.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }

    /// Relative to the Environment's Workspace, as every path either driver takes is.
    pub fn exec(&mut self, command: &[&str]) -> io::Result<Streaming> {
        self.provisioned.exec(command)
    }

    pub fn read_file(&mut self, path: &str) -> io::Result<Vec<u8>> {
        self.provisioned.read_file(path)
    }

    pub fn write_file(&mut self, path: &str, contents: &[u8]) -> io::Result<()> {
        self.provisioned.write_file(path, contents)
    }

    /// `None` while the Environment is still running.
    pub fn status(&mut self) -> io::Result<Option<Exited>> {
        self.provisioned.status()
    }

    pub fn destroy(mut self) -> io::Result<()> {
        let destroyed = self.provisioned.destroy();
        self.destroyed = true;

        destroyed
    }
}

impl Drop for Environment {
    /// A caller that panics before destroying must not leave an Environment behind either.
    fn drop(&mut self) {
        if !self.destroyed {
            let _ = self.provisioned.destroy();
        }
    }
}

/// What `exec` hands back while the command is still running, so its output can be read as it
/// arrives rather than only once it is over.
pub struct Streaming {
    child: Child,
}

impl Streaming {
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Waits, collecting whichever of the two streams was not taken.
    pub fn finish(self) -> io::Result<Finished> {
        let finished = self.child.wait_with_output()?;

        Ok(Finished {
            exited: finished.status.into(),
            out: String::from_utf8_lossy(&finished.stdout).trim().to_owned(),
            err: String::from_utf8_lossy(&finished.stderr).trim().to_owned(),
        })
    }
}

#[derive(Debug)]
pub struct Finished {
    pub exited: Exited,
    pub out: String,
    pub err: String,
}

/// How something ended, in the only terms both drivers have: a container reports an exit code
/// and nothing else, and a process killed by a signal reports none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exited(Option<i32>);

impl Exited {
    pub const fn with(code: i32) -> Self {
        Self(Some(code))
    }

    pub const fn without_a_code() -> Self {
        Self(None)
    }

    pub fn success(&self) -> bool {
        self.0 == Some(0)
    }
}

impl From<ExitStatus> for Exited {
    fn from(status: ExitStatus) -> Self {
        Self(status.code())
    }
}

impl fmt::Display for Exited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(code) => write!(f, "with the code {code}"),
            None => f.write_str("without a code"),
        }
    }
}
