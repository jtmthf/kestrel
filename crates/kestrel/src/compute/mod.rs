//! The local-exec `Compute` driver: an Environment as a local process tree (ADR-0005). The
//! escape hatch that exists whether or not it is planned, and the primary test seam's
//! Environment (0.1/03).

use std::io;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

pub struct Environment {
    child: Child,
    #[cfg(unix)]
    pgid: i32,
    destroyed: bool,
}

impl Environment {
    pub fn name(&self) -> String {
        format!("local-exec/{}", self.child.id())
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// `None` while the Environment is still running.
    pub fn status(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

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
}

impl Drop for Environment {
    /// A test that panics before calling `destroy` must not leave an orphan behind either.
    fn drop(&mut self) {
        if self.destroyed {
            return;
        }
        let _ = self.kill_tree();
        let _ = self.child.wait();
    }
}

pub struct LocalExec;

impl LocalExec {
    pub fn provision(
        &self,
        program: &Path,
        args: &[&str],
        variables: &[(&str, &str)],
    ) -> io::Result<Environment> {
        let mut command = Command::new(program);
        command
            .args(args)
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

        let child = command.spawn()?;

        Ok(Environment {
            #[cfg(unix)]
            pgid: child.id() as i32,
            child,
            destroyed: false,
        })
    }

    pub fn destroy(&self, mut environment: Environment) -> io::Result<()> {
        environment.kill_tree()?;
        environment.child.wait()?;
        environment.destroyed = true;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::time::{Duration, Instant};

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

    /// Spawns a shell that backgrounds a grandchild `sleep` and prints its pid, so the test
    /// can prove the whole tree died rather than only the process this Environment holds
    /// onto directly.
    fn spawn_tree_with_grandchild(driver: &LocalExec) -> (Environment, i32) {
        let mut environment = driver
            .provision(Path::new("sh"), &["-c", "sleep 30 & echo $!; wait"], &[])
            .expect("sh should spawn");

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
        let driver = LocalExec;
        let (environment, grandchild) = spawn_tree_with_grandchild(&driver);

        driver.destroy(environment).expect("destroy should succeed");

        eventually_gone(grandchild);
    }

    #[test]
    fn a_dropped_environment_leaves_no_orphan_process_even_without_explicit_destroy() {
        let driver = LocalExec;
        let grandchild = {
            let (environment, grandchild) = spawn_tree_with_grandchild(&driver);
            drop(environment);
            grandchild
        };

        eventually_gone(grandchild);
    }
}
