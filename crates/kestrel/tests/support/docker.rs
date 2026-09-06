//! The docker CLI as a test drives it: one place an invocation goes through, and one place a
//! failure says what it was doing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug)]
pub struct Ran {
    pub code: i32,
    pub out: String,
    pub err: String,
}

pub fn ran(arguments: &[&str]) -> Ran {
    let ran = Command::new("docker")
        .current_dir(repository())
        .args(arguments)
        .output()
        .expect("docker should be reachable");

    Ran {
        code: ran.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&ran.stdout).trim().to_owned(),
        err: String::from_utf8_lossy(&ran.stderr).trim().to_owned(),
    }
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
pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two directories under the repository")
        .to_path_buf()
}
