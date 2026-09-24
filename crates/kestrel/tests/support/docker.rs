//! The docker CLI as a test drives it: one place an invocation goes through, and one place a
//! failure says what it was doing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

#[derive(Debug)]
pub struct Ran {
    pub code: i32,
    pub out: String,
    pub err: String,
}

pub fn ran(arguments: &[&str]) -> Ran {
    ran_against(&[], arguments)
}

/// `ran` with the given variables in the docker process's environment rather than the test
/// process's own, which is how the compose suite names the resources a checkout owns.
pub fn ran_against(variables: &[(&str, &str)], arguments: &[&str]) -> Ran {
    let mut command = docker();
    for (key, value) in variables {
        command.env(key, value);
    }
    let ran = command
        .args(arguments)
        .output()
        .expect("docker should be reachable");

    Ran {
        code: ran.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&ran.stdout).trim().to_owned(),
        err: String::from_utf8_lossy(&ran.stderr).trim().to_owned(),
    }
}

fn docker() -> Command {
    let mut command = Command::new("docker");
    command.current_dir(repository());
    command
}

pub fn completed(arguments: &[&str], doing: &str) -> String {
    let ran = ran(arguments);
    assert_eq!(ran.code, 0, "{doing} failed:\n{}", ran.err);

    ran.out
}

/// Run instead of whatever the image would otherwise start.
pub fn running(image: &str, command: &[&str]) -> Ran {
    let (program, arguments) = command.split_first().expect("a command to run");
    let mut run = vec!["run", "--rm", "--entrypoint", program, image];
    run.extend_from_slice(arguments);

    ran(&run)
}

pub fn configured(image: &str, field: &str) -> String {
    completed(
        &["image", "inspect", "--format", field, image],
        "inspecting the image",
    )
}

pub fn removed(name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "--force", "--volumes", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Every image here builds from the repository root, so that is where a test invokes docker.
/// Read when the test runs, not when it compiled: checkouts sharing a target directory run
/// whichever binary compiled last, and each must still build and tag its own source.
pub fn repository() -> PathBuf {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);

    manifest
        .ancestors()
        .nth(2)
        .expect("the crate sits two directories under the repository")
        .to_path_buf()
}

pub fn checkout_digest(checkout: &Path) -> String {
    let canonical = std::fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    let mut hashed = Sha256::new();
    hashed.update(canonical.to_string_lossy().as_bytes());

    kestrel::hex::encode(&hashed.finalize())[..16].to_owned()
}
