//! A repository a Workspace can name that is on this machine rather than on a forge, so a Run
//! that checks its Workspace out reaches nothing over the network.

use std::process::{Command, Stdio};
use std::sync::OnceLock;

use tempfile::TempDir;

pub const NAME: &str = "kestrel";
pub const BRANCH: &str = "main";

pub fn url() -> &'static str {
    static REPOSITORY: OnceLock<(TempDir, String)> = OnceLock::new();

    &REPOSITORY.get_or_init(initialized).1
}

fn initialized() -> (TempDir, String) {
    let directory = TempDir::new().expect("a temporary directory");
    let repository = directory.path().join(NAME);
    std::fs::create_dir(&repository).expect("the repository should be made");
    std::fs::write(repository.join("README.md"), "a workspace's repository\n")
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
        let ran = Command::new("git")
            .args(&arguments)
            .current_dir(&repository)
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

    let url = format!("file://{}", repository.display());

    (directory, url)
}
