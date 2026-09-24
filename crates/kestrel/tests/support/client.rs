//! The installed `kestrel` Client as an operator runs it: its own process, handed a
//! control-plane URL and nothing else, in a home and a working directory holding no database.

use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::fd::FromRawFd as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use super::{Harness, built};

const PATIENCE: Duration = Duration::from_secs(30);

pub fn binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();

    BINARY.get_or_init(|| built::named("kestrel-client", "kestrel"))
}

pub struct Client {
    child: Child,
    stdout: Receiver<String>,
    stderr: thread::JoinHandle<String>,
    home: TempDir,
    invocation: Invocation,
}

pub struct Finished {
    pub status: ExitStatus,
    pub out: Vec<String>,
    pub err: String,
    /// Whatever the Client left in the only directories it was given.
    pub left_behind: Vec<PathBuf>,
}

#[derive(Clone, Default)]
pub struct Invocation {
    input: Option<String>,
    environment: Vec<(String, String)>,
    files: Vec<(PathBuf, String)>,
    clones: Vec<(String, PathBuf)>,
    within: PathBuf,
}

impl Invocation {
    pub fn given(mut self, input: &str) -> Self {
        self.input = Some(input.to_owned());
        self
    }

    pub fn env(mut self, name: &str, value: &str) -> Self {
        self.environment.push((name.to_owned(), value.to_owned()));
        self
    }

    /// A file placed in the Client's home before it runs, at a path relative to that home.
    pub fn file(mut self, path: &str, contents: &str) -> Self {
        self.files.push((PathBuf::from(path), contents.to_owned()));
        self
    }

    /// A `git clone` of this repository made in the Client's home before it runs, with the
    /// PATH it needs to read the clone the way an operator's shell would.
    pub fn cloned(mut self, repository: &str, directory: &str) -> Self {
        self.clones
            .push((repository.to_owned(), PathBuf::from(directory)));
        self.env(
            "PATH",
            &std::env::var("PATH").expect("a PATH to find git on"),
        )
    }

    /// Runs from this directory inside the home rather than the home itself.
    pub fn within(mut self, directory: &str) -> Self {
        self.within = PathBuf::from(directory);
        self
    }

    fn placed(&self, entry: &Path) -> bool {
        self.files
            .iter()
            .map(|(path, _)| path)
            .chain(self.clones.iter().map(|(_, directory)| directory))
            .chain([&self.within])
            .any(|path| path.components().next() == Some(Component::Normal(entry.as_os_str())))
    }
}

impl Client {
    pub fn spawn(control_plane: &str, args: &[&str]) -> Self {
        Self::spawn_as(control_plane, args, Invocation::default())
    }

    pub fn spawn_as(control_plane: &str, args: &[&str], invocation: Invocation) -> Self {
        let home = TempDir::new().expect("a temporary home");
        for (path, contents) in &invocation.files {
            let path = home.path().join(path);
            if let Some(directory) = path.parent() {
                std::fs::create_dir_all(directory).expect("the file's directory should create");
            }
            std::fs::write(&path, contents).expect("the file should write");
        }
        for (repository, directory) in &invocation.clones {
            let cloned = Command::new("git")
                .arg("clone")
                .arg("--quiet")
                .arg(repository)
                .arg(home.path().join(directory))
                .output()
                .expect("git should be reachable");
            assert!(
                cloned.status.success(),
                "cloning {repository} failed:\n{}",
                String::from_utf8_lossy(&cloned.stderr)
            );
        }
        let working = home.path().join(&invocation.within);
        std::fs::create_dir_all(&working).expect("the working directory should create");

        let mut child = Command::new(binary())
            .args(args)
            .current_dir(&working)
            .env_clear()
            .env("HOME", home.path())
            .env("KESTREL_CONTROL_PLANE", control_plane)
            .envs(invocation.environment.iter().cloned())
            .stdin(if invocation.input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the client should spawn");
        if let Some(input) = &invocation.input {
            let mut stdin = child.stdin.take().expect("stdin should be piped");
            stdin
                .write_all(input.as_bytes())
                .expect("the input should reach the client");
        }

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
            invocation,
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
            .map(|entry| entry.expect("an entry"))
            .filter(|entry| !self.invocation.placed(Path::new(&entry.file_name())))
            .map(|entry| entry.path())
            .collect();

        Finished {
            status,
            out,
            err,
            left_behind,
        }
    }
}

impl Finished {
    pub fn records(&self) -> Vec<Value> {
        assert!(self.status.success(), "the client failed:\n{}", self.err);
        self.out
            .iter()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("{line} is not a record: {error}"))
            })
            .collect()
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

pub fn ran_given(control_plane: &str, args: &[&str], input: &str) -> Finished {
    ran_as(control_plane, args, Invocation::default().given(input))
}

pub fn ran_as(control_plane: &str, args: &[&str], invocation: Invocation) -> Finished {
    Client::spawn_as(control_plane, args, invocation).finish()
}

/// Runs off the async runtime, so a test can await it beside the control plane it addresses.
pub async fn ran_by(harness: &Harness, args: &[&str], invocation: Invocation) -> Finished {
    let operator = harness.operator();
    let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();

    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        ran_as(&operator, &args, invocation)
    })
    .await
    .expect("the client should run")
}

/// What a Client shows someone watching: stdout and stderr as a terminal interleaves them,
/// on a terminal exactly this many columns wide.
pub struct Shown {
    pub status: ExitStatus,
    pub said: String,
}

impl Shown {
    pub fn lines(&self) -> Vec<&str> {
        self.said.lines().collect()
    }
}

/// A pty rather than a pipe, because what the Client decides is exactly what it finds there.
pub fn ran_on_a_terminal(control_plane: &str, args: &[&str], columns: u16, typed: &str) -> Shown {
    let home = TempDir::new().expect("a temporary home");
    let (controller, device) = a_terminal(columns);
    let mut child = {
        let mut command = Command::new(binary());
        command
            .args(args)
            .current_dir(home.path())
            .env_clear()
            .env("HOME", home.path())
            .env("KESTREL_CONTROL_PLANE", control_plane)
            .stdin(Stdio::from(
                device.try_clone().expect("the device should clone"),
            ))
            .stdout(Stdio::from(
                device.try_clone().expect("the device should clone"),
            ))
            .stderr(Stdio::from(device));
        command.spawn().expect("the client should spawn")
    };

    let mut typing = controller.try_clone().expect("the controller should clone");
    typing
        .write_all(typed.as_bytes())
        .expect("the typing should reach the client");
    let said = thread::spawn(move || read_until_hangup(controller));
    let status = child.wait().expect("the client should be waited on");

    Shown {
        status,
        said: said
            .join()
            .expect("the terminal should drain")
            .replace('\r', ""),
    }
}

fn a_terminal(columns: u16) -> (File, File) {
    // `ptsname` answers one buffer for the whole process, so two tests opening a terminal at
    // once would otherwise be handed the same device name.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _in_turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);

    #[allow(unsafe_code)]
    let (controller, device) = unsafe {
        let controller = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(controller >= 0, "a pseudo-terminal would not open");
        assert_eq!(libc::grantpt(controller), 0, "the device would not grant");
        assert_eq!(libc::unlockpt(controller), 0, "the device would not unlock");

        let name = libc::ptsname(controller);
        assert!(!name.is_null(), "the device has no name");
        let device = libc::open(name, libc::O_RDWR | libc::O_NOCTTY);
        assert!(device >= 0, "the device would not open");

        let size = libc::winsize {
            ws_row: 24,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            libc::ioctl(controller, libc::TIOCSWINSZ as _, &raw const size),
            0,
            "the terminal would not take a width"
        );

        (File::from_raw_fd(controller), File::from_raw_fd(device))
    };

    (controller, device)
}

/// The last device closing reads as an error on Linux and as an end on macOS, and both of
/// them are the Client having exited.
fn read_until_hangup(mut controller: File) -> String {
    let mut said = Vec::new();
    let mut buffer = [0; 4096];

    loop {
        match controller.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => said.extend_from_slice(&buffer[..read]),
        }
    }

    String::from_utf8_lossy(&said).into_owned()
}
