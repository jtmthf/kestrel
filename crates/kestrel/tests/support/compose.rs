//! The shipped compose stack as a test drives it: built and brought up the way the README
//! says to, driven by the installed Client through the port it publishes, and torn down with
//! its volume. Every resource the suite touches is scoped to a namespace derived from this
//! checkout, so two checkouts on one daemon never address the same project, volume, network,
//! image or host port.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::client::{self, Finished, Invocation};
use super::docker::{Ran, ran_against, repository};
use super::images;

pub const CONTROL_PLANE: &str = "kestrel";
pub const FILTER: &str = "socket-proxy";
const PATIENCE: Duration = Duration::from_secs(60);

/// Everything one checkout names its stack with, so no two checkouts on one daemon address
/// the same resource, and the control plane is told the names that are its own.
pub struct Namespace {
    pub project: String,
    pub volume: String,
    pub link: String,
    pub control_plane: String,
    pub environment: String,
}

impl Namespace {
    /// What a `docker compose` invocation must inherit for every resource it touches to be
    /// this checkout's. The operator port is left for the daemon to choose, so no two
    /// checkouts' stacks contend for the host's.
    pub fn environment(&self) -> [(&str, &str); 6] {
        [
            ("COMPOSE_PROJECT_NAME", &self.project),
            ("KESTREL_VOLUME", &self.volume),
            ("KESTREL_LINK_NETWORK", &self.link),
            ("KESTREL_CONTROL_IMAGE", &self.control_plane),
            ("KESTREL_ENV_IMAGE", &self.environment),
            ("KESTREL_OPERATOR_PORT", ""),
        ]
    }
}

/// The namespace this checkout's suite runs under: one per canonical repository path, so a
/// second process in the same checkout derives the same names and a second checkout never
/// collides with it.
fn namespace() -> &'static Namespace {
    static NAMESPACE: OnceLock<Namespace> = OnceLock::new();
    NAMESPACE.get_or_init(|| namespace_for(&repository()))
}

/// The namespace one checkout's suite runs under, from the checkout's canonical path: the
/// same path derives the same names, and a different path derives none of the same ones.
pub fn namespace_for(checkout: &Path) -> Namespace {
    let canonical = std::fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    let mut hashed = Sha256::new();
    hashed.update(canonical.to_string_lossy().as_bytes());
    let digest = &kestrel::hex::encode(&hashed.finalize())[..16];

    Namespace {
        project: format!("kestrel-{digest}"),
        volume: format!("kestrel-{digest}"),
        link: format!("kestrel-{digest}-link"),
        control_plane: format!("kestrel-{digest}"),
        environment: format!("kestrel-{digest}-env"),
    }
}

/// The kernel holds this for the life of the process, so two suites from this same checkout
/// wait rather than mutate its stack at once, and one checkout's suite never sees another's
/// process-local lock. Another checkout derives another file name, so its suite waits for
/// nothing.
fn host_lock() -> &'static File {
    static HELD: OnceLock<File> = OnceLock::new();
    HELD.get_or_init(|| {
        let path = std::env::temp_dir().join(format!("{}.compose.lock", namespace().project));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap_or_else(|error| panic!("{} could not open: {error}", path.display()));

        #[allow(unsafe_code)]
        let held = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        assert_eq!(
            held,
            0,
            "{} would not hold an exclusive lock",
            path.display()
        );

        file
    })
}

/// Two suites from one checkout at once would fight over its names, and a mutex is visible to
/// one binary only: the host-visible lock spans binaries, this one spans the tests in one.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

pub struct Stack {
    _one_at_a_time: MutexGuard<'static, ()>,
}

impl Stack {
    pub fn up() -> Self {
        let stack = Self {
            _one_at_a_time: ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner),
        };
        built();
        stack.down_with_its_volume();
        stack.start();

        stack
    }

    /// Down and up again the way an operator restarts one: the volume the database is on is
    /// what `down` leaves behind.
    pub fn comes_back(&self) {
        completed(&["down"], "bringing the stack down");
        self.start();
    }

    /// Where the installed Client reaches the stack: the port it published on the host's
    /// loopback, which moves each time the stack comes up.
    pub fn operator(&self) -> String {
        let published = completed(
            &["port", CONTROL_PLANE, "7718"],
            "finding the published operator port",
        );

        format!("http://{published}")
    }

    pub fn client(&self, command: &[&str]) -> Finished {
        client::ran(&self.operator(), command)
    }

    pub fn ran(&self, command: &[&str]) -> String {
        self.ran_given(command, None)
    }

    pub fn ran_given(&self, command: &[&str], input: Option<&str>) -> String {
        let invocation = input.map_or_else(Invocation::default, |input| {
            Invocation::default().given(input)
        });
        let ran = client::ran_as(&self.operator(), command, invocation);
        assert!(
            ran.status.success(),
            "`kestrel {}` against the stack failed:\n{}",
            command.join(" "),
            ran.err
        );

        ran.out.join("\n")
    }

    pub fn in_the_control_plane(&self, command: &[&str]) -> Ran {
        let mut exec = vec!["exec", "--no-TTY", CONTROL_PLANE];
        exec.extend_from_slice(command);

        ran(&exec)
    }

    /// A throwaway container where an Environment would be: the same image, on the same
    /// network, reaching for whatever a test hands it.
    pub fn on_the_link_an_environment_dials(&self, command: &[&str]) -> Ran {
        let (program, arguments) = command.split_first().expect("a command to run");
        let mut run = vec![
            "run",
            "--rm",
            "--network",
            &namespace().link,
            "--entrypoint",
            program,
            &namespace().environment,
        ];
        run.extend_from_slice(arguments);

        super::docker::ran(&run)
    }

    pub fn everything_a_service_said(&self, service: &str) -> String {
        let said = ran(&["logs", "--no-color", service]);

        format!("{}\n{}", said.out, said.err)
    }

    fn start(&self) {
        completed(&["up", "--detach", "--wait"], "bringing the stack up");
    }

    fn down_with_its_volume(&self) {
        completed(
            &["down", "--volumes", "--remove-orphans"],
            "bringing the stack down",
        );
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.down_with_its_volume();
    }
}

/// Every image the compose file names. A run CI built for has both images pushed tagged by
/// commit, so this pulls them into the checkout's namespace rather than building what a sibling
/// job already built; a local run builds them, once for every test in this binary.
pub fn built() -> &'static [String] {
    static BUILT: OnceLock<Vec<String>> = OnceLock::new();

    BUILT.get_or_init(|| {
        host_lock();
        match (
            images::sourced(images::ENV),
            images::sourced(images::CONTROL_PLANE),
        ) {
            (Some(environment), Some(control_plane)) => {
                pulled(&environment, &control_plane);
            }
            _ => {
                completed(&["build"], "building the images the compose file names");
            }
        }

        completed(&["config", "--images"], "listing the images")
            .lines()
            .map(str::to_owned)
            .collect()
    })
}

/// What CI built is named for the repository and the commit, and what the compose file names is
/// this checkout's namespace, so each is tagged into it rather than referenced directly: the
/// compose file keeps naming the images an operator's stack uses.
fn pulled(environment: &str, control_plane: &str) {
    let namespace = namespace();
    for (source, named) in [
        (environment, &namespace.environment),
        (control_plane, &namespace.control_plane),
    ] {
        let tagged = super::docker::ran(&["tag", source, named]);
        assert_eq!(
            tagged.code, 0,
            "tagging {source} as {named} failed:\n{}",
            tagged.err
        );
    }
}

fn rendered(variables: &[(&str, &str)]) -> Ran {
    let mut rendering = Command::new("docker");
    rendering.current_dir(repository()).env_clear();
    for kept in ["PATH", "HOME"] {
        if let Ok(value) = std::env::var(kept) {
            rendering.env(kept, value);
        }
    }
    for (key, value) in variables {
        rendering.env(key, value);
    }
    let output = rendering
        .args(["compose", "config", "--format", "json"])
        .output()
        .expect("docker should be reachable");

    Ran {
        code: output.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        err: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}

/// The compose file rendered with nothing in the environment but a path to docker and the
/// context it reads: what an operator has to supply shows up here as a warning.
pub fn rendered_against_an_empty_environment() -> Ran {
    rendered(&[])
}

/// The compose file rendered the way this checkout's suite runs it: every resource the
/// control plane addresses points into this checkout's namespace.
pub fn rendered_with_the_checkout_namespace() -> Ran {
    rendered(&namespace().environment())
}

pub fn until<T>(what: &str, ready: impl Fn() -> Option<T>) -> T {
    let deadline = Instant::now() + PATIENCE;

    loop {
        if let Some(ready) = ready() {
            return ready;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn completed(arguments: &[&str], doing: &str) -> String {
    let ran = ran(arguments);
    assert_eq!(ran.code, 0, "{doing} failed:\n{}", ran.err);

    ran.out
}

fn ran(arguments: &[&str]) -> Ran {
    let mut compose = vec!["compose"];
    compose.extend_from_slice(arguments);

    ran_against(&namespace().environment(), &compose)
}
