//! The escape hatch that exists whether or not it is planned (ADR-0005), and the Instance the
//! primary test seam provisions: a directory, and a supervisor process per Run inside it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

use super::{Exited, Instance, Provisioned, Streaming, Supervising, Supervisor};
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

    pub(super) fn provision(&self, run: RunId) -> io::Result<Instance> {
        let name = format!("kestrel-{run}");
        fs::create_dir_all(within(&name))?;
        fs::create_dir_all(home(&name))?;

        Ok(self.instance(name))
    }

    pub(super) fn resume(&self, instance: &str) -> io::Result<Option<Instance>> {
        let name = named(instance)?;
        if !within(name).is_dir() {
            return Ok(None);
        }

        Ok(Some(self.instance(name.to_owned())))
    }

    pub(super) fn destroy_named(&self, instance: &str) -> io::Result<()> {
        let name = named(instance)?;
        removed(&home(name))?;
        removed(&within(name))
    }

    fn instance(&self, name: String) -> Instance {
        Instance {
            provisioned: Box::new(Directory {
                supervisor: self.supervisor.clone(),
                workspace: within(&name),
                home: home(&name),
            }),
            name: format!("local-exec/{name}"),
        }
    }
}

fn within(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

/// The agent's home, apart from the operator's: a Subscription Profile's files are written
/// beneath it and removed again, which in the operator's own home would take their login with it.
fn home(name: &str) -> PathBuf {
    within(&format!("{name}.home"))
}

fn named(instance: &str) -> io::Result<&str> {
    instance
        .strip_prefix("local-exec/")
        .filter(|name| !name.is_empty() && !name.contains(['/', '\\']) && *name != "..")
        .ok_or_else(|| io::Error::other(format!("{instance} is not a local instance")))
}

struct Directory {
    supervisor: PathBuf,
    workspace: PathBuf,
    home: PathBuf,
}

impl Directory {
    fn at(&self, path: &str) -> PathBuf {
        self.workspace.join(path)
    }
}

impl Provisioned for Directory {
    fn exec(&mut self, command: &[&str]) -> io::Result<Streaming> {
        let (program, arguments) = command
            .split_first()
            .ok_or_else(|| io::Error::other("nothing to exec in the instance"))?;

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

    fn supervise(&mut self, variables: &[(&str, &str)]) -> io::Result<Supervisor> {
        let mut command = Command::new(&self.supervisor);
        fs::create_dir_all(&self.home)?;
        command
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
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

        let mut child = command.spawn()?;

        Ok(Supervisor {
            name: format!("local-exec/{}", child.id()),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            supervising: Box::new(Process {
                #[cfg(unix)]
                pgid: child.id() as i32,
                child,
            }),
            stopped: false,
        })
    }

    fn destroy(&mut self) -> io::Result<()> {
        removed(&self.home)?;
        removed(&self.workspace)
    }
}

struct Process {
    child: Child,
    #[cfg(unix)]
    pgid: i32,
}

impl Supervising for Process {
    fn status(&mut self) -> io::Result<Option<Exited>> {
        Ok(self.child.try_wait()?.map(Exited::from))
    }

    fn stop(&mut self) -> io::Result<()> {
        // Darwin refuses to signal a group whose only member left is its unreaped leader, which
        // is what `stop_named` reaping it first leaves behind.
        #[cfg(unix)]
        if let Err(error) = killed(self.pgid) {
            self.child.kill().map_err(|_| error)?;
        }
        #[cfg(not(unix))]
        self.child.kill()?;

        self.child.wait().map(drop)
    }
}

/// Every process in the tree, not only the one the supervisor is.
#[cfg(unix)]
fn killed(pgid: i32) -> io::Result<()> {
    #[allow(unsafe_code)]
    let killed = unsafe { libc::killpg(pgid, libc::SIGKILL) };
    if killed == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }

    Ok(())
}

fn removed(workspace: &Path) -> io::Result<()> {
    match fs::remove_dir_all(workspace) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        removed => removed,
    }
}

pub(super) fn stop_named(supervisor: &str) -> io::Result<()> {
    let pid: i32 = supervisor
        .strip_prefix("local-exec/")
        .ok_or_else(|| io::Error::other(format!("{supervisor} is not a local supervisor")))?
        .parse()
        .map_err(|error| io::Error::other(format!("{supervisor} has no process id: {error}")))?;

    #[cfg(unix)]
    killed(pid)?;
    #[cfg(not(unix))]
    let _ = pid;

    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::super::Driver;
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
        panic!("pid {pid} is still alive 5s after its tree should have been stopped");
    }

    fn driver(scripts: &TempDir, shell: &str) -> Driver {
        let script = scripts.path().join("supervisor");
        fs::write(&script, format!("#!/bin/sh\n{shell}\n")).expect("a script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .expect("an executable script");

        Driver::LocalExec(LocalExec::running(&script))
    }

    fn provisioned(driver: &Driver) -> Instance {
        driver
            .provision(RunId::generate())
            .expect("the instance should provision")
    }

    /// A shell that backgrounds a grandchild `sleep` and prints its pid, so the test can prove
    /// the whole tree died rather than only the process the supervisor handle holds directly.
    fn a_tree_with_a_grandchild(instance: &mut Instance) -> (Supervisor, i32) {
        let mut supervisor = instance
            .supervise(&[])
            .expect("the supervisor should start");

        let stdout = supervisor.take_stdout().expect("stdout should be piped");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("the grandchild's pid should print");
        let grandchild: i32 = line.trim().parse().expect("a pid");

        (supervisor, grandchild)
    }

    #[test]
    fn stopping_a_supervisor_leaves_no_orphan_process_in_its_tree() {
        let scripts = TempDir::new().expect("a temporary directory");
        let driver = driver(&scripts, "sleep 30 & echo $!\nwait");
        let mut instance = provisioned(&driver);
        let (supervisor, grandchild) = a_tree_with_a_grandchild(&mut instance);

        supervisor.stop().expect("stop should succeed");

        eventually_gone(grandchild);
        instance.destroy().expect("destroy should succeed");
    }

    #[test]
    fn a_dropped_supervisor_leaves_no_orphan_process_even_without_an_explicit_stop() {
        let scripts = TempDir::new().expect("a temporary directory");
        let driver = driver(&scripts, "sleep 30 & echo $!\nwait");
        let mut instance = provisioned(&driver);
        let grandchild = {
            let (supervisor, grandchild) = a_tree_with_a_grandchild(&mut instance);
            drop(supervisor);
            grandchild
        };

        eventually_gone(grandchild);
        instance.destroy().expect("destroy should succeed");
    }

    #[test]
    fn a_supervisor_stopped_by_name_takes_its_tree_with_it() {
        let scripts = TempDir::new().expect("a temporary directory");
        let driver = driver(&scripts, "sleep 30 & echo $!\nwait");
        let mut instance = provisioned(&driver);
        let (supervisor, grandchild) = a_tree_with_a_grandchild(&mut instance);

        driver
            .stop_named(supervisor.name())
            .expect("stop should succeed");

        eventually_gone(grandchild);
        drop(supervisor);
        instance.destroy().expect("destroy should succeed");
    }

    #[test]
    fn what_a_run_wrote_outlives_its_supervisor_and_goes_with_the_instance() {
        let scripts = TempDir::new().expect("a temporary directory");
        let driver = driver(&scripts, "echo what an agent left behind > left");
        let mut instance = provisioned(&driver);
        let name = instance.name().to_owned();

        let mut supervisor = instance
            .supervise(&[])
            .expect("the supervisor should start");
        while supervisor
            .status()
            .expect("the status should read")
            .is_none()
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        supervisor.stop().expect("stop should succeed");

        let mut resumed = driver
            .resume(&name)
            .expect("the instance should resume")
            .expect("the instance should still be there");
        assert_eq!(
            resumed.read_file("left").expect("the file should read"),
            b"what an agent left behind\n"
        );

        resumed.destroy().expect("destroy should succeed");
        assert!(
            driver
                .resume(&name)
                .expect("a gone instance is not an error")
                .is_none(),
            "the instance outlived being destroyed"
        );
    }

    #[test]
    fn a_command_execs_in_the_workspace_and_streams_what_it_says() {
        let scripts = TempDir::new().expect("a temporary directory");
        let mut instance = provisioned(&driver(&scripts, "sleep 30"));
        instance
            .write_file("read-me", b"in the workspace")
            .expect("the file should write");

        let finished = instance
            .exec(&["cat", "read-me"])
            .expect("cat should exec")
            .finish()
            .expect("cat should finish");

        assert!(finished.exited.success(), "cat said {finished:?}");
        assert_eq!(finished.out, "in the workspace");

        instance.destroy().expect("destroy should succeed");
    }

    #[test]
    fn a_supervisor_that_is_still_running_has_no_status_and_one_that_ended_has_the_code() {
        let scripts = TempDir::new().expect("a temporary directory");
        let mut instance = provisioned(&driver(&scripts, "exit 3"));
        let mut supervisor = instance
            .supervise(&[])
            .expect("the supervisor should start");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match supervisor.status().expect("the status should read") {
                Some(exited) => {
                    assert_eq!(exited, Exited::with(3));
                    break;
                }
                None => assert!(Instant::now() < deadline, "the supervisor never exited"),
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        supervisor.stop().expect("stop should succeed");
        instance.destroy().expect("destroy should succeed");
    }
}
