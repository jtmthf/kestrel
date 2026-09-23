//! The `kestrel` control-plane image as a test drives it: the one CI built for this change, or
//! one built here when nothing named it, run over the volume an operator's database lives on,
//! and reached from outside it.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::client;
use super::docker::{self, Ran, removed};
use super::images;

const LOCAL: &str = "kestrel:test";
pub const DATABASE: &str = "/var/lib/kestrel/kestrel.db";
const DATA_DIR: &str = "/var/lib/kestrel";
const PATIENCE: Duration = Duration::from_secs(30);

/// The image a test runs: what CI built for this change and named, or one built here for local
/// runs.
pub fn built() -> &'static str {
    static BUILT: OnceLock<String> = OnceLock::new();

    BUILT.get_or_init(|| match images::sourced(images::CONTROL_PLANE) {
        Some(image) => image,
        None => {
            docker::completed(
                &[
                    "build",
                    "--file",
                    "images/kestrel/Dockerfile",
                    "--tag",
                    LOCAL,
                    ".",
                ],
                "building the image",
            );
            LOCAL.to_owned()
        }
    })
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

/// The image started as its roles, with the link and the operator boundary each on a host
/// port, so a test reaches them the way an Environment and an operator outside it would.
pub struct Started {
    name: String,
    link: String,
    operator: String,
}

impl Started {
    pub fn with(volume: &Volume, arguments: &[&str]) -> Self {
        let name = named("control-plane");
        let mut run = vec![
            "run",
            "--detach",
            "--name",
            &name,
            "--env",
            "RUST_LOG=info",
            "--publish",
            "127.0.0.1::7717",
            "--publish",
            "127.0.0.1::7718",
            "--volume",
            &volume.mount,
            built(),
        ];
        run.extend_from_slice(arguments);
        docker::completed(&run, "starting the control plane");

        let published = |port: &str| {
            docker::completed(
                &["port", &name, port],
                &format!("finding the address {port} was published on"),
            )
        };
        let link = published("7717/tcp");
        let operator = format!("http://{}", published("7718/tcp"));

        Self {
            name,
            link,
            operator,
        }
    }

    /// Where a Client reaches the operator boundary, once it answers one. A published port
    /// accepts a connection before anything in the container listens, so connecting proves
    /// nothing.
    pub fn operator(&self) -> &str {
        let deadline = Instant::now() + PATIENCE;
        while !client::ran(&self.operator, &["organization", "list"])
            .status
            .success()
        {
            assert!(
                Instant::now() < deadline,
                "the operator boundary never answered at {}. it said:\n{}",
                self.operator,
                self.everything_it_said()
            );
            std::thread::sleep(Duration::from_millis(100));
        }

        &self.operator
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
        let mut link = TcpStream::connect(&self.link).expect("the published link should accept");
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
