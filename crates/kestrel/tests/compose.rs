//! The shipped `compose.yaml`: the one command someone other than the maintainer runs, and the
//! filtered socket proxy the daemon is reached through (ADR-0009).
//!
//! Every test here builds images and brings a stack up on the host daemon, which a `cargo
//! test` has no business doing on its own, so they are ignored by default and CI runs them
//! with `--ignored`. They share one project name and one volume, so they run one at a time.

mod support;

use support::compose::{self, CONTROL_PLANE, FILTER, Stack};
use support::docker;
use support::image::Container;

const REPOSITORY: &str = "https://github.com/jtmthf/kestrel";

#[test]
#[ignore = "builds images and brings a stack up"]
fn one_command_brings_up_a_working_kestrel() {
    let stack = Stack::up();

    let said = stack.everything_a_service_said(CONTROL_PLANE);

    assert!(
        said.contains("role=serve") && said.contains("role=work"),
        "the control plane did not start its roles. it said:\n{said}"
    );
    let organization = stack.ran(&["organization", "declare", "acme"]);
    assert_eq!(
        stack.ran(&["organization", "list"]),
        format!("{organization}  acme")
    );
}

#[test]
#[ignore = "renders the compose file with docker"]
fn the_operator_supplies_nothing() {
    let rendered = compose::rendered_against_an_empty_environment();

    assert_eq!(
        rendered.code, 0,
        "the compose file does not render with nothing set:\n{}",
        rendered.err
    );
    assert!(
        !rendered.err.contains("is not set"),
        "the compose file wants a value an operator has to supply:\n{}",
        rendered.err
    );
}

#[test]
#[ignore = "builds the images the compose file names"]
fn the_stack_is_the_control_plane_the_filter_and_the_image_a_run_executes_in() {
    let mut images = compose::built()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    images.sort_unstable();

    let [control_plane, environment, filter] = images[..] else {
        panic!("the compose file ships {images:?}, and it ships three images");
    };
    assert_eq!(control_plane, "kestrel");
    assert_eq!(environment, "kestrel-env");
    assert!(
        filter.contains("socket-proxy") && filter.contains("@sha256:"),
        "the filter is not a socket proxy pinned by digest: {filter}"
    );
    for image in images {
        assert_eq!(
            docker::ran(&["image", "inspect", "--format", "{{.Id}}", image]).code,
            0,
            "{image} is not on the daemon after a build"
        );
    }
}

/// The control plane holds no socket at all: what it reaches over `DOCKER_HOST` is the filter,
/// and a request outside the filter's list never reaches the daemon.
#[test]
#[ignore = "builds images and brings a stack up"]
fn the_daemon_is_reached_through_the_filter_rather_than_by_its_socket() {
    let stack = Stack::up();

    let socket = stack.in_the_control_plane(&["test", "-e", "/var/run/docker.sock"]);

    assert_ne!(
        socket.code, 0,
        "the control plane holds the docker socket the filter exists to keep from it"
    );
}

/// The filter is on a network the control plane joins and nothing else does, so an agent that
/// reaches past kestrel for the daemon finds nothing listening rather than a filter to probe.
#[test]
#[ignore = "builds images and brings a stack up"]
fn nothing_an_environment_runs_can_reach_the_filter() {
    let stack = Stack::up();

    let reached = stack.on_the_link_an_environment_dials(&[
        "bash",
        "-c",
        "exec 3<>/dev/tcp/socket-proxy/2375",
    ]);

    assert_ne!(
        reached.code, 0,
        "an environment reached the filter: {reached:?}"
    );
}

#[test]
#[ignore = "builds images and brings a stack up"]
fn an_operation_outside_the_filter_is_refused_and_the_refusal_says_what_it_was() {
    let stack = Stack::up();

    let pulled = stack.in_the_control_plane(&["docker", "pull", "busybox"]);
    let mounted = stack.in_the_control_plane(&[
        "docker",
        "create",
        "--name",
        "escaped",
        "--volume",
        "/:/host",
        "kestrel-env",
    ]);

    for refused in [&pulled, &mounted] {
        assert_ne!(refused.code, 0, "the filter allowed {refused:?}");
        assert!(
            refused.err.contains("Forbidden"),
            "an unhelpful refusal: {refused:?}"
        );
    }

    let filter = stack.everything_a_service_said(FILTER);
    assert!(
        filter.contains(r#"reason="path not allowed""#) && filter.contains("/images/create"),
        "the filter did not say what it refused pulling an image. it said:\n{filter}"
    );
    assert!(
        filter.contains("bind mount source directory not allowed: /"),
        "the filter did not say what it refused mounting the host. it said:\n{filter}"
    );
}

/// Every request the driver makes goes through the filter, so a Run that reaches an
/// Environment and leaves none behind is the whole list exercised.
#[tokio::test]
#[ignore = "builds images and brings a stack up"]
async fn a_run_provisions_and_destroys_an_environment_through_the_filter() {
    let stack = Stack::up();
    let session = a_session(&stack);
    let run = stack.ran(&["run", "enqueue", "--session", &session]);

    let environment = compose::until("the run to reach an environment", || {
        listed(&stack, &session, &run).environment
    });
    let container = Container::named(&environment);
    assert_eq!(environment, format!("docker/kestrel-{run}"));

    // The link an Environment dials is a container beside it rather than the host's gateway,
    // so reaching it at all is the network the control plane put it on.
    compose::until("the environment to reach the link", || {
        container
            .everything_it_said()
            .contains("link open")
            .then_some(())
    });

    // The credential this stack holds reaches no provider, so what ends this Run is the control
    // plane stopping under it rather than anything the agent did.
    stack.comes_back();

    container.is_gone().await;
    assert_eq!(
        listed(&stack, &session, &run).went,
        "failed: the control plane stopped while this run was in flight"
    );
}

#[test]
#[ignore = "builds images and brings a stack up"]
fn the_stack_comes_back_up_with_every_session_it_had() {
    let stack = Stack::up();
    let session = a_session(&stack);
    let shown = stack.ran(&["session", "show", &session]);
    let transcript = stack.ran(&["session", "transcript", &session]);

    stack.comes_back();

    assert_eq!(stack.ran(&["session", "show", &session]), shown);
    assert_eq!(stack.ran(&["session", "transcript", &session]), transcript);
    assert!(
        !transcript.is_empty(),
        "nothing was transcribed for the restart to keep"
    );
}

/// The commands `USAGE.md` walks a reader through, minus the two its neighbours already cover:
/// `a_run_provisions_and_destroys_an_environment_through_the_filter` covers enqueueing a Run, and
/// `the_stack_comes_back_up_with_every_session_it_had` covers surviving a restart.
#[test]
#[ignore = "builds images and brings a stack up"]
fn the_commands_usage_documents_are_the_commands_that_work() {
    let stack = Stack::up();
    let session = a_session(&stack);

    let shown = stack.ran(&["session", "show", &session]);
    for line in [
        format!("session       {session}"),
        "organization  acme".to_owned(),
        "workspace     kestrel".to_owned(),
        "agent         builder".to_owned(),
        "state         open".to_owned(),
        "opened        ".to_owned(),
    ] {
        assert!(
            shown.contains(&line),
            "USAGE.md shows `{line}`, and `session show` said:\n{shown}"
        );
    }

    let transcript = stack.in_the_control_plane(&["kestrel", "session", "transcript", &session]);
    assert!(
        transcript.out.contains("participant joined  builder"),
        "USAGE.md shows the Agent joining as the first entry, and the transcript was:\n{transcript:?}"
    );
    assert!(
        transcript.err.contains(&format!("cursor  {session}:1")),
        "USAGE.md shows the cursor on stderr, and the transcript was:\n{transcript:?}"
    );

    stack.ran(&["session", "seal", &session]);

    assert!(
        stack
            .ran(&["session", "show", &session])
            .contains("state         sealed"),
        "USAGE.md says sealing is visible on the Session, and it was not"
    );
}

fn a_session(stack: &Stack) -> String {
    stack.ran(&["organization", "declare", "acme"]);
    stack.ran(&[
        "workspace",
        "declare",
        "kestrel",
        "--organization",
        "acme",
        "--repository",
        REPOSITORY,
        "--branch",
        "main",
    ]);
    // The Agent names no model, so the agent runtime's own default is what a Run would get.
    stack.ran(&[
        "agent",
        "declare",
        "builder",
        "--organization",
        "acme",
        "--model",
        "",
    ]);
    // A Run reaches no model without one. This value reaches no provider either, which is why
    // nothing here gets further than an Environment.
    stack.held_a_provider_credential("acme", "OPENCODE_API_KEY", "not-a-key");

    stack.ran(&[
        "session",
        "open",
        "--organization",
        "acme",
        "--workspace",
        "kestrel",
        "--agent",
        "builder",
    ])
}

struct Listed {
    environment: Option<String>,
    went: String,
}

/// A Run as `run list` shows it: the Environment it is in, and how it went.
fn listed(stack: &Stack, session: &str, run: &str) -> Listed {
    let listed = stack.ran(&["run", "list", "--session", session]);
    let line = listed
        .lines()
        .find(|line| line.starts_with(run))
        .unwrap_or_else(|| panic!("{run} is not among the session's runs:\n{listed}"));
    let [_, environment, went] = line.split("  ").collect::<Vec<_>>()[..] else {
        panic!("a run is listed as {line:?}");
    };

    Listed {
        environment: (environment != "-").then(|| environment.to_owned()),
        went: went.to_owned(),
    }
}
