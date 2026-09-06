//! The `kestrel` control-plane image as a test drives it: built rather than assumed present,
//! and run over the volume an operator's database lives on.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::docker::{self, Ran, removed};

const IMAGE: &str = "kestrel:test";
pub const DATABASE: &str = "/var/lib/kestrel/kestrel.db";
const DATA_DIR: &str = "/var/lib/kestrel";
const PATIENCE: Duration = Duration::from_secs(30);

pub fn built() -> &'static str {
    static BUILT: OnceLock<()> = OnceLock::new();

    BUILT.get_or_init(|| {
        docker::completed(
            &[
                "build",
                "--file",
                "images/kestrel/Dockerfile",
                "--tag",
                IMAGE,
                ".",
            ],
            "building the image",
        );
    });

    IMAGE
}

/// Run instead of the control plane the image would otherwise start.
pub fn running(command: &[&str]) -> Ran {
    docker::running(built(), command)
}

pub fn configured(field: &str) -> String {
    docker::configured(built(), field)
}

/// Where kestrel's database lives: the one thing a container replacement leaves behind.
pub struct Volume {
    name: String,
    mount: String,
}

impl Volume {
    pub fn empty() -> Self {
        let name = named("volume");
        docker::completed(&["volume", "create", &name], "creating a volume");
        let mount = format!("{name}:{DATA_DIR}");

        Self { name, mount }
    }

    /// The CLI role over this volume, which does its one thing and exits.
    pub fn ran(&self, command: &[&str]) -> String {
        let ran = self.run(command);
        assert_eq!(
            ran.code,
            0,
            "`kestrel {}` in the image failed:\n{}",
            command.join(" "),
            ran.err
        );

        ran.out
    }

    pub fn run(&self, command: &[&str]) -> Ran {
        let mut run = vec!["run", "--rm", "--volume", &self.mount, built()];
        run.extend_from_slice(command);

        docker::ran(&run)
    }

    pub fn holds(&self, path: &str) -> bool {
        docker::ran(&[
            "run",
            "--rm",
            "--volume",
            &self.mount,
            "--entrypoint",
            "test",
            built(),
            "-f",
            path,
        ])
        .code
            == 0
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let _ = docker::ran(&["volume", "rm", "--force", &self.name]);
    }
}

/// The image started as a role rather than as a one-shot command.
pub struct Started {
    name: String,
    published: Option<String>,
}

impl Started {
    pub fn with(volume: &Volume, arguments: &[&str]) -> Self {
        Self::starting(volume, arguments, false)
    }

    /// The link on a host port, so a test reaches it the way an Environment outside the
    /// container would.
    pub fn publishing_the_link(volume: &Volume, arguments: &[&str]) -> Self {
        Self::starting(volume, arguments, true)
    }

    fn starting(volume: &Volume, arguments: &[&str], publish: bool) -> Self {
        let name = named("control-plane");
        let mut run = vec!["run", "--detach", "--name", &name, "--env", "RUST_LOG=info"];
        if publish {
            run.extend_from_slice(&["--publish", "127.0.0.1::7717"]);
        }
        run.extend_from_slice(&["--volume", &volume.mount, built()]);
        run.extend_from_slice(arguments);
        docker::completed(&run, "starting the control plane");

        let published = publish.then(|| {
            docker::completed(
                &["port", &name, "7717/tcp"],
                "finding the address the link was published on",
            )
        });

        Self { name, published }
    }

    pub fn wait_until_it_says(&self, what: &str) {
        let deadline = Instant::now() + PATIENCE;

        while Instant::now() < deadline {
            if self.said(what) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        panic!(
            "timed out waiting for the control plane to say {what:?}. it said:\n{}",
            self.everything_it_said()
        );
    }

    pub fn said(&self, what: &str) -> bool {
        self.everything_it_said().contains(what)
    }

    pub fn everything_it_said(&self) -> String {
        let logs = docker::ran(&["logs", &self.name]);

        format!("{}\n{}", logs.out, logs.err)
    }

    /// What the link answers a request with, or nothing at all when the address it bound is
    /// not one a caller outside the container can reach.
    pub fn what_the_link_answers(&self) -> String {
        let address = self
            .published
            .as_deref()
            .expect("the link should have been published on a host port");
        let mut link = TcpStream::connect(address).expect("the published link should accept");
        link.write_all(b"GET / HTTP/1.0\r\n\r\n")
            .expect("the published link should take a request");

        let mut answered = String::new();
        let _ = link.read_to_string(&mut answered);

        answered
    }

    /// A `SIGTERM` and the wait for it, so what the role said on the way down is in the logs
    /// before anything reads them.
    pub fn stop(&self) {
        docker::completed(&["stop", &self.name], "stopping the control plane");
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        removed(&self.name);
    }
}

fn named(what: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    format!(
        "kestrel-test-{what}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
