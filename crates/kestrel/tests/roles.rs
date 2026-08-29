// Off unix this compiles to no tests rather than failing ones: a non-Linux CI runner added
// later would be green without proving anything.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const PATIENCE: Duration = Duration::from_secs(30);

/// stderr is drained on a thread so the pipe cannot fill and block the child. The data
/// directory is held for the process's lifetime so concurrent tests never race over the
/// real default database.
struct Kestrel {
    child: Child,
    stderr: Receiver<String>,
    seen: Vec<String>,
    _data_dir: TempDir,
}

impl Kestrel {
    fn spawn(args: &[&str]) -> Self {
        let data_dir = TempDir::new().expect("a temporary data directory");
        let mut child = Command::new(env!("CARGO_BIN_EXE_kestrel"))
            .args(args)
            .env("RUST_LOG", "info")
            .env("KESTREL_DATA_DIR", data_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("kestrel should spawn");

        let pipe = child.stderr.take().expect("stderr should be piped");
        let (lines, stderr) = channel();
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });

        Self {
            child,
            stderr,
            seen: Vec::new(),
            _data_dir: data_dir,
        }
    }

    fn wait_for(&mut self, what: &str, predicate: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if self.seen.iter().any(|line| predicate(line)) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.stderr.recv_timeout(remaining) {
                Ok(line) => self.seen.push(line),
                Err(RecvTimeoutError::Timeout) => {
                    panic!(
                        "timed out waiting for {what}. stderr so far:\n{}",
                        self.stderr_so_far()
                    )
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!(
                        "kestrel exited before {what}. stderr:\n{}",
                        self.stderr_so_far()
                    )
                }
            }
        }
    }

    fn wait_until_role_started(&mut self, role: &str) {
        self.wait_for(&format!("the {role} role to start"), |line| {
            line.contains("role started") && line.contains(&format!("role={role}"))
        });
    }

    fn assert_role_stopped(&mut self, role: &str) {
        assert!(
            self.seen
                .iter()
                .any(|line| line.contains("role stopped") && line.contains(&format!("role={role}"))),
            "the {role} role never reported stopping. stderr:\n{}",
            self.stderr_so_far()
        );
    }

    fn signal(&self, signal: i32) {
        #[allow(unsafe_code)]
        let sent = unsafe { libc::kill(self.child.id() as i32, signal) };
        assert_eq!(sent, 0, "kill({signal}) failed");
    }

    fn shut_down_cleanly(&mut self, signal: i32) {
        self.signal(signal);
        let status = self.child.wait().expect("kestrel should be waitable");
        while let Ok(line) = self.stderr.recv_timeout(Duration::from_secs(5)) {
            self.seen.push(line);
        }
        assert!(
            status.success(),
            "kestrel exited with {status}. stderr:\n{}",
            self.stderr_so_far()
        );
    }

    fn assert_role_never_started(&self, role: &str) {
        assert!(
            !self.stderr_so_far().contains(&format!("role={role}")),
            "the {role} role started, and this selection should not have started it. stderr:\n{}",
            self.stderr_so_far()
        );
    }

    fn stderr_so_far(&self) -> String {
        self.seen.join("\n")
    }
}

impl Drop for Kestrel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn no_role_starts_every_role_in_one_process_and_stops_cleanly_on_sigterm() {
    let mut kestrel = Kestrel::spawn(&[]);

    kestrel.wait_until_role_started("serve");
    kestrel.wait_until_role_started("work");
    kestrel.shut_down_cleanly(libc::SIGTERM);

    kestrel.assert_role_stopped("serve");
    kestrel.assert_role_stopped("work");
}

#[test]
fn no_role_stops_cleanly_on_sigint() {
    let mut kestrel = Kestrel::spawn(&[]);

    kestrel.wait_until_role_started("serve");
    kestrel.shut_down_cleanly(libc::SIGINT);
}

#[test]
fn serve_starts_only_the_serve_role() {
    let mut kestrel = Kestrel::spawn(&["serve"]);

    kestrel.wait_until_role_started("serve");
    kestrel.shut_down_cleanly(libc::SIGTERM);

    kestrel.assert_role_never_started("work");
}

#[test]
fn work_starts_only_the_work_role() {
    let mut kestrel = Kestrel::spawn(&["work"]);

    kestrel.wait_until_role_started("work");
    kestrel.shut_down_cleanly(libc::SIGTERM);

    kestrel.assert_role_never_started("serve");
}
