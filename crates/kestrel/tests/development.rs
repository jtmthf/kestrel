//! The `kestrel-dev` image: everything an agent working on Kestrel itself needs, and nothing
//! it would have to sign in with.
//!
//! Every test here builds and runs the image, which a `cargo test` has no business doing on
//! its own, so they are ignored by default. CI runs all but `kestrel_passes_its_own_checks`,
//! which compiles the workspace three times over and runs on a schedule instead.

mod support;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use support::docker::{self, removed};
use support::image;

const PATIENCE: Duration = Duration::from_secs(60);

#[test]
#[ignore = "builds and runs the kestrel-dev image"]
fn the_toolchain_git_and_gh_are_each_invocable_in_the_image() {
    let rustc = running(&["rustc", "--version"]);
    assert!(
        rustc.out.starts_with("rustc 1.96.0"),
        "rustc in the image is not the toolchain rust-toolchain.toml names: {rustc:?}"
    );

    for command in [
        &["cargo", "--version"][..],
        &["cargo", "fmt", "--version"],
        &["cargo", "clippy", "--version"],
        &["git", "--version"],
        &["gh", "--version"],
    ] {
        let ran = running(command);
        assert_eq!(ran.code, 0, "{command:?} in the image said {ran:?}");
    }
}

#[test]
#[ignore = "builds and runs the kestrel-dev image"]
fn each_harness_answers_an_acp_handshake_in_the_image() {
    for runtime in [
        &["claude-agent-acp"][..],
        &["codex-acp"],
        &["opencode", "acp"],
    ] {
        let answer = initialized(runtime);
        assert_eq!(
            answer["result"]["protocolVersion"], 1,
            "{runtime:?} answered initialize with {answer}"
        );
    }
}

#[test]
#[ignore = "builds and runs the kestrel-dev image"]
fn the_image_carries_no_credentials() {
    let variables = docker::configured(image::development(), "{{json .Config.Env}}");
    let names: Vec<Value> = serde_json::from_str(&variables).expect("the image's environment");
    for name in names.iter().filter_map(Value::as_str) {
        let name = name.split('=').next().unwrap_or_default().to_uppercase();
        assert!(
            !["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"]
                .iter()
                .any(|word| name.contains(word)),
            "the image sets {name}"
        );
    }

    let home = running(&[
        "find",
        "/home/kestrel",
        "-mindepth",
        "1",
        "!",
        "-name",
        ".bashrc",
        "!",
        "-name",
        ".profile",
        "!",
        "-name",
        ".bash_logout",
    ]);
    assert_eq!(home.code, 0, "sweeping the home directory said {home:?}");
    assert!(
        home.out.is_empty(),
        "a login would be found in what the image puts in its home directory:\n{}",
        home.out
    );
}

#[test]
#[ignore = "builds kestrel-dev and compiles the workspace in it"]
fn kestrel_passes_its_own_checks_in_the_image() {
    let checkout = format!("{}:/workspace/kestrel:ro", docker::repository().display());
    let ran = docker::ran(&[
        "run",
        "--rm",
        "--volume",
        &checkout,
        "--workdir",
        "/workspace/kestrel",
        "--env",
        "CARGO_TARGET_DIR=/tmp/target",
        "--entrypoint",
        "sh",
        image::development(),
        "-c",
        "set -e
         cargo fmt --all --check
         cargo clippy --locked --workspace --all-targets -- -D warnings
         cargo build --locked --workspace
         cargo test --locked --workspace",
    ]);

    assert_eq!(
        ran.code, 0,
        "kestrel failed its own checks in the image:\n{}\n{}",
        ran.out, ran.err
    );
}

fn running(command: &[&str]) -> docker::Ran {
    docker::running(image::development(), command)
}

/// What an ACP runtime in the image answers the first message a client sends, with no
/// credential anywhere it could look.
fn initialized(runtime: &[&str]) -> Value {
    let (program, arguments) = runtime.split_first().expect("a runtime to spawn");
    let name = format!("kestrel-dev-handshake-{}-{}", program, std::process::id());
    removed(&name);

    let mut spawned = Command::new("docker")
        .args(["run", "--rm", "--interactive", "--name", &name])
        .args(["--entrypoint", program, image::development()])
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("docker should run the image");

    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": false, "writeTextFile": false },
                "terminal": false,
            },
        },
    });
    let mut stdin = spawned.stdin.take().expect("the runtime's stdin is piped");
    writeln!(stdin, "{initialize}").expect("the runtime should read its stdin");

    let stdout = spawned
        .stdout
        .take()
        .expect("the runtime's stdout is piped");
    let (answered, answer) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str::<Value>(&line)
                && message["id"] == 0
            {
                let _ = answered.send(message);
                return;
            }
        }
    });

    let answer = answer.recv_timeout(PATIENCE);
    drop(stdin);
    removed(&name);
    let _ = spawned.wait();

    answer.unwrap_or_else(|_| panic!("{runtime:?} never answered initialize"))
}
