//! A repository a Project can name that is on this machine rather than on a forge, so a Run
//! that checks its Project out reaches nothing over the network.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use tempfile::TempDir;

pub const NAME: &str = "kestrel";
pub const BRANCH: &str = "main";
pub const EXISTING_BRANCH: &str = "kestrel/existing";
/// A second repository, for a Project that declares more than one.
pub const OTHER: &str = "companion";

pub fn url() -> &'static str {
    static REPOSITORY: OnceLock<(TempDir, String)> = OnceLock::new();

    &REPOSITORY.get_or_init(initialized).1
}

pub fn other_url() -> &'static str {
    static REPOSITORY: OnceLock<(TempDir, String)> = OnceLock::new();

    &REPOSITORY
        .get_or_init(|| {
            let directory = TempDir::new().expect("a temporary directory");
            let repository = directory.path().join(OTHER);
            std::fs::create_dir(&repository).expect("the repository should be made");
            std::fs::write(repository.join("README.md"), "another repository\n")
                .expect("the repository should have something in it");
            for arguments in [
                vec!["init", "--initial-branch", BRANCH],
                vec!["add", "README.md"],
                vec![
                    "-c",
                    "user.name=kestrel",
                    "-c",
                    "user.email=kestrel@example.com",
                    "commit",
                    "--message",
                    "the commit the branch points at",
                ],
            ] {
                git(&repository, &arguments);
            }
            let url = format!("file://{}", repository.display());

            (directory, url)
        })
        .1
}

fn initialized() -> (TempDir, String) {
    let directory = TempDir::new().expect("a temporary directory");
    let repository = directory.path().join(NAME);
    std::fs::create_dir(&repository).expect("the repository should be made");
    std::fs::write(repository.join("README.md"), "a project's repository\n")
        .expect("the repository should have something in it");

    let committed = |message| {
        vec![
            "-c",
            "user.name=kestrel",
            "-c",
            "user.email=kestrel@example.com",
            "commit",
            "--all",
            "--message",
            message,
        ]
    };
    for arguments in [
        vec!["init", "--initial-branch", BRANCH],
        vec!["add", "README.md"],
        committed("the commit the branch points at"),
        vec!["checkout", "-b", EXISTING_BRANCH],
    ] {
        git(&repository, &arguments);
    }
    std::fs::write(repository.join("README.md"), "an existing branch's work\n")
        .expect("the existing branch should have work of its own");
    git(&repository, &committed("the work on an existing branch"));
    git(&repository, &["checkout", BRANCH]);

    let url = format!("file://{}", repository.display());

    (directory, url)
}

fn git(repository: &Path, arguments: &[&str]) {
    let ran = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("git should be reachable");
    assert!(
        ran.status.success(),
        "`git {}` failed:\n{}",
        arguments.join(" "),
        String::from_utf8_lossy(&ran.stderr)
    );
}
