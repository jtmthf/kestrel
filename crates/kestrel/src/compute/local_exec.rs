//! The escape hatch that exists whether or not it is planned (ADR-0005), and the Environment
//! the primary test seam provisions.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

use super::{Environment, Exited, Provisioned, Streaming};
use crate::domain::RunId;

#[derive(Debug, Clone)]
pub struct LocalExec {
    supervisor: PathBuf,
}

impl LocalExec {
    pub fn running(supervisor: impl Into<PathBuf>) -> Self {
        Self {
            supervisor: supervisor.into(),
        }
    }

    pub(super) fn provision(
        &self,
        run: RunId,
        variables: &[(&str, &str)],
    ) -> io::Result<Environment> {
        let workspace = std::env::temp_dir().join(format!("kestrel-{run}"));
        fs::create_dir_all(&workspace)?;

        let mut command = Command::new(&self.supervisor);
        command
            .current_dir(&workspace)
            .envs(variables.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(unix)]
        {
            // A fresh session makes this process its own process-group leader, so every
            // child it forks inherits the same group and `killpg` reaches all of them.
            #[allow(unsafe_code)]
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_dir_all(&workspace);
                return Err(error);
            }
        };

        let name = format!("local-exec/{}", child.id());
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let process = Process {
            #[cfg(unix)]
            pgid: child.id() as i32,
            child,
            workspace,
        };

        Ok(Environment {
            name,
            stdout,
            stderr,
            provisioned: Box::new(process),
            destroyed: false,
        })
    }
}

struct Process {
    child: Child,
    #[cfg(unix)]
    pgid: i32,
    workspace: PathBuf,
}

impl Process {
    /// Kills every process in the tree, not only the one this Environment spawned directly.
    fn kill_tree(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            #[allow(unsafe_code)]
            let killed = unsafe { libc::killpg(self.pgid, libc::SIGKILL) };
            if killed == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        #[cfg(not(unix))]
        self.child.kill()?;

        Ok(())
    }

    fn at(&self, path: &str) -> PathBuf {
        self.workspace.join(path)
    }
}

impl Provisioned for Process {
    fn exec(&mut self, command: &[&str]) -> io::Result<Streaming> {
        let (program, arguments) = command
            .split_first()
            .ok_or_else(|| io::Error::other("nothing to exec in the environment"))?;

        let child = Command::new(program)
            .args(arguments)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        Ok(Streaming { child })
    }

    fn read_file(&mut self, path: &str) -> io::Result<Vec<u8>> {
        fs::read(self.at(path))
    }

    fn write_file(&mut self, path: &str, contents: &[u8]) -> io::Result<()> {
        let path = self.at(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, contents)
    }

    fn status(&mut self) -> io::Result<Option<Exited>> {
        Ok(self.child.try_wait()?.map(Exited::from))
    }

    fn destroy(&mut self) -> io::Result<()> {
        self.kill_tree()?;
        self.child.wait()?;

        removed(&self.workspace)
    }
}

/// Nothing a Run wrote survives it (ADR-0005), so the Workspace goes with the Environment.
fn removed(workspace: &Path) -> io::Result<()> {
    match fs::remove_dir_all(workspace) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        removed => removed,
    }
}

pub(super) fn destroy_named(run: RunId, environment: &str) -> io::Result<()> {
    let pid: i32 = environment
        .strip_prefix("local-exec/")
        .ok_or_else(|| io::Error::other(format!("{environment} is not a local environment")))?
        .parse()
        .map_err(|error| io::Error::other(format!("{environment} has no process id: {error}")))?;

    #[cfg(unix)]
    {
        #[allow(unsafe_code)]
        let killed = unsafe { libc::killpg(pid, libc::SIGKILL) };
        if killed == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
    }

    removed(&std::env::temp_dir().join(format!("kestrel-{run}")))
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::*;

    fn process_exists(pid: i32) -> bool {
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(pid, 0) == 0
        }
    }

    fn eventually_gone(pid: i32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !process_exists(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("pid {pid} is still alive 5s after its tree should have been destroyed");
    }

    fn provisioned(scripts: &TempDir, run: RunId, shell: &str) -> Environment {
        let script = scripts.path().join("environment");
        fs::write(&script, format!("#!/bin/sh\n{shell}\n")).expect("a script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .expect("an executable script");

        LocalExec::running(&script)
            .provision(run, &[])
            .expect("the environment should provision")
    }

    /// A shell that backgrounds a grandchild `sleep` and prints its pid, so the test can prove
    /// the whole tree died rather than only the process this Environment holds onto directly.
    fn a_tree_with_a_grandchild(scripts: &TempDir) -> (Environment, i32) {
        let mut environment = provisioned(scripts, RunId::generate(), "sleep 30 & echo $!\nwait");

        let stdout = environment.take_stdout().expect("stdout should be piped");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("the grandchild's pid should print");
        let grandchild: i32 = line.trim().parse().expect("a pid");

        (environment, grandchild)
    }

    #[test]
    fn destroying_an_environment_leaves_no_orphan_process_in_its_tree() {
        let scripts = TempDir::new().expect("a temporary directory");
        let (environment, grandchild) = a_tree_with_a_grandchild(&scripts);

        environment.destroy().expect("destroy should succeed");

        eventually_gone(grandchild);
    }

    #[test]
    fn a_dropped_environment_leaves_no_orphan_process_even_without_explicit_destroy() {
        let scripts = TempDir::new().expect("a temporary directory");
        let grandchild = {
            let (environment, grandchild) = a_tree_with_a_grandchild(&scripts);
            drop(environment);
            grandchild
        };

        eventually_gone(grandchild);
    }

    #[test]
    fn a_file_written_into_the_workspace_reads_back_and_is_gone_with_the_environment() {
        let scripts = TempDir::new().expect("a temporary directory");
        let run = RunId::generate();
        let mut environment = provisioned(&scripts, run, "sleep 30");

        environment
            .write_file("kestrel/README.md", b"what an agent left behind")
            .expect("the file should write");
        assert_eq!(
            environment
                .read_file("kestrel/README.md")
                .expect("the file should read"),
            b"what an agent left behind"
        );

        let workspace = std::env::temp_dir().join(format!("kestrel-{run}"));
        assert!(workspace.exists());
        environment.destroy().expect("destroy should succeed");
        assert!(
            !workspace.exists(),
            "the workspace outlived the environment it belonged to"
        );
    }

    #[test]
    fn a_command_execs_in_the_workspace_and_streams_what_it_says() {
        let scripts = TempDir::new().expect("a temporary directory");
        let mut environment = provisioned(&scripts, RunId::generate(), "sleep 30");
        environment
            .write_file("read-me", b"in the workspace")
            .expect("the file should write");

        let finished = environment
            .exec(&["cat", "read-me"])
            .expect("cat should exec")
            .finish()
            .expect("cat should finish");

        assert!(finished.exited.success(), "cat said {finished:?}");
        assert_eq!(finished.out, "in the workspace");

        environment.destroy().expect("destroy should succeed");
    }

    #[test]
    fn an_environment_that_is_still_running_has_no_status_and_one_that_ended_has_the_code() {
        let scripts = TempDir::new().expect("a temporary directory");
        let mut environment = provisioned(&scripts, RunId::generate(), "exit 3");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match environment.status().expect("the status should read") {
                Some(exited) => {
                    assert_eq!(exited, Exited::with(3));
                    break;
                }
                None => assert!(Instant::now() < deadline, "the environment never exited"),
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        environment.destroy().expect("destroy should succeed");
    }
}
