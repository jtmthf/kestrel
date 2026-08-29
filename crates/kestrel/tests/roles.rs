//! The binary, spawned and signalled.
//!
//! "Starts every role in one process and shuts down cleanly on a signal" is not observable
//! from inside the process, so this is the one seam where the test spawns `kestrel` itself.

// Every target in ADR-0006 is a Linux container, and there is no signal here to send off
// unix. On a non-unix host this file compiles to no tests at all rather than to failing
// ones — so a Windows runner added to CI later would be green without proving anything.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

/// Generous: a debug binary on a loaded CI runner still starts well inside it, and a hang
/// fails the test rather than the suite.
const PATIENCE: Duration = Duration::from_secs(30);

/// A spawned `kestrel`, with its stderr drained on a thread so the pipe cannot fill and
/// block the child while the test is waiting on something else.
struct Kestrel {
    child: Child,
    stderr: Receiver<String>,
    seen: Vec<String>,
}

impl Kestrel {
    fn spawn(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kestrel"))
            .args(args)
            .env("RUST_LOG", "info")
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
        }
    }

    /// Blocks until a stderr line satisfies `predicate`, or fails the test.
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
        // The only way to test signal handling is to send a signal.
        #[allow(unsafe_code)]
        let sent = unsafe { libc::kill(self.child.id() as i32, signal) };
        assert_eq!(sent, 0, "kill({signal}) failed");
    }

    /// Signals, waits for exit, drains whatever stderr was still in flight, and asserts
    /// that kestrel treated the signal as a request to stop rather than as a failure.
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

    /// Named for what it is rather than `log`: `Log` is a port, and a Session's Transcript
    /// is a different thing entirely (CONTEXT.md, ADR-0005).
    fn stderr_so_far(&self) -> String {
        self.seen.join("\n")
    }
}

impl Drop for Kestrel {
    fn drop(&mut self) {
        // A test that failed part-way must not leave a kestrel running.
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
