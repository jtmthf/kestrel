//! Binaries this suite drives, built rather than assumed present, so a Rust test never passes
//! against a stale artifact someone built by hand.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn binary(package: &str) -> PathBuf {
    let alongside = alongside_this_test();
    let profile = alongside
        .file_name()
        .and_then(|profile| profile.to_str())
        .expect("a named profile directory");
    let built = Command::new(env!("CARGO"))
        .args([
            "build",
            "--package",
            package,
            "--profile",
            if profile == "debug" { "dev" } else { profile },
        ])
        .output()
        .expect("cargo should build");

    assert!(
        built.status.success(),
        "building {package} failed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );

    let binary = alongside.join(package);
    assert!(
        binary.exists(),
        "cargo built {package}, but not to {}",
        binary.display()
    );
    binary
}

fn alongside_this_test() -> PathBuf {
    std::env::current_exe()
        .expect("a test binary should know where it is")
        .parent()
        .and_then(Path::parent)
        .expect("the test binary sits in the profile's deps directory")
        .to_path_buf()
}
