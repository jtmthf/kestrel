//! `kestrel-client` as an operator runs it: its own process, handed a control-plane URL and
//! nothing else, in a home and a working directory holding no database.

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::built;

const PATIENCE: Duration = Duration::from_secs(30);

pub fn binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();

    BINARY.get_or_init(|| built::binary("kestrel-client"))
}

pub struct Client {
    child: Child,
    stdout: Receiver<String>,
    stderr: thread::JoinHandle<String>,
    home: TempDir,
}

pub struct Finished {
    pub status: ExitStatus,
    pub out: Vec<String>,
    pub err: String,
    /// Whatever the Client left in the only directories it was given.
    pub left_behind: Vec<PathBuf>,
}

impl Client {
    pub fn spawn(control_plane: &str, args: &[&str]) -> Self {
        let home = TempDir::new().expect("a temporary home");
        let mut child = Command::new(binary())
            .args(args)
            .current_dir(home.path())
            .env_clear()
            .env("HOME", home.path())
            .env("KESTREL_CONTROL_PLANE", control_plane)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the client should spawn");

        let pipe = child.stdout.take().expect("stdout should be piped");
        let (lines, stdout) = channel();
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
        let mut pipe = child.stderr.take().expect("stderr should be piped");
        let stderr = thread::spawn(move || {
            let mut said = String::new();
            let _ = pipe.read_to_string(&mut said);
            said
        });

        Self {
            child,
            stdout,
            stderr,
            home,
        }
    }

    pub fn line(&mut self) -> String {
        match self.stdout.recv_timeout(PATIENCE) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => panic!("the client printed nothing in time"),
            Err(RecvTimeoutError::Disconnected) => {
                let finished = self.finish();
                panic!(
                    "the client exited {} before printing a line:\n{}",
                    finished.status, finished.err
                )
            }
        }
    }

    pub fn finish(&mut self) -> Finished {
        let deadline = Instant::now() + PATIENCE;
        let status = loop {
            if let Some(status) = self
                .child
                .try_wait()
                .expect("the client should be waited on")
            {
                break status;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("the client did not exit in time");
            }
            thread::sleep(Duration::from_millis(20));
        };

        let out = self.stdout.iter().collect();
        let err = std::mem::replace(&mut self.stderr, thread::spawn(String::new))
            .join()
            .expect("stderr should drain");
        let left_behind = std::fs::read_dir(self.home.path())
            .expect("the home should list")
            .map(|entry| entry.expect("an entry").path())
            .collect();

        Finished {
            status,
            out,
            err,
            left_behind,
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Runs to completion, printing whatever it prints.
pub fn ran(control_plane: &str, args: &[&str]) -> Finished {
    Client::spawn(control_plane, args).finish()
}
