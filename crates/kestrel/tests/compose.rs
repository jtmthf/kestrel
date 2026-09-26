//! The shipped `compose.yaml`: the one command someone other than the maintainer runs, and the
//! filtered socket proxy the daemon is reached through (ADR-0009).
//!
//! Every test here builds images and brings a stack up on the host daemon, which a `cargo
//! test` has no business doing on its own, so they are ignored by default and CI runs them
//! with `--ignored`. A checkout's stack owns a namespace derived from its repository path, so
//! checkouts on one daemon never tear down one another's stack, and a host-visible lock keeps
//! one checkout's suites from running over each other.

mod support;

use std::path::Path;

use serde_json::Value;

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
        format!("{organization}\tacme\t-")
    );
}

/// The compose file must be the stable operator-facing stack with nothing set: one project,
/// one volume, one link network and two images, under the names an operator already knows.
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

    let model = model(rendered);
    assert_eq!(model["name"], "kestrel");
    assert_eq!(model["volumes"]["kestrel"]["name"], "kestrel");
    assert_eq!(model["networks"]["link"]["name"], "kestrel-link");
    assert_eq!(model["services"]["kestrel"]["image"], "kestrel");
    assert_eq!(model["services"]["kestrel-env"]["image"], "kestrel-env");
    assert_eq!(
        model["services"]["kestrel"]["environment"]["KESTREL_NETWORK"],
        "kestrel-link"
    );
    assert_eq!(
        model["services"]["kestrel"]["environment"]["KESTREL_IMAGE"],
        "kestrel-env"
    );
    assert_eq!(
        model["services"]["kestrel"]["ports"],
        serde_json::json!([{
            "mode": "ingress",
            "host_ip": "127.0.0.1",
            "target": 7718,
            "published": "7718",
            "protocol": "tcp",
        }])
    );
}

/// The operator boundary authenticates nobody (ADR-0015), so the port it is published on is
/// the host's loopback in every rendering, and the healthcheck asks it over HTTP rather than
/// running a Client the image does not carry.
#[test]
#[ignore = "renders the compose file with docker"]
fn the_operator_boundary_is_published_on_the_hosts_loopback_and_probed_over_http() {
    let model = model(compose::rendered_with_the_checkout_namespace());
    let kestrel = &model["services"]["kestrel"];

    let ports = kestrel["ports"].as_array().expect("published ports");
    assert_eq!(ports.len(), 1, "{ports:?}");
    assert_eq!(ports[0]["host_ip"], "127.0.0.1");
    assert_eq!(ports[0]["target"], 7718);
    let probe = kestrel["healthcheck"]["test"]
        .as_array()
        .expect("a healthcheck command");
    assert_eq!(probe[..2], [Value::from("CMD"), Value::from("curl")]);
    assert!(
        probe
            .iter()
            .any(|argument| argument == "http://127.0.0.1:7718/operator/organizations"),
        "{probe:?}"
    );
}

/// Rendered the way this checkout's suite runs it, every resource the control plane addresses
/// — the project, the volume it keeps its database on, the link network it hands the daemon,
/// and the image a Run executes in — is one of this checkout's.
#[test]
#[ignore = "renders the compose file with docker"]
fn a_checkout_namespaces_every_resource_its_stack_runs() {
    let namespace = compose::namespace_for(&docker::repository());
    let rendered = compose::rendered_with_the_checkout_namespace();

    assert_eq!(
        rendered.code, 0,
        "the compose file does not render for this checkout:\n{}",
        rendered.err
    );

    let model = model(rendered);
    assert_eq!(model["name"], namespace.project);
    assert_eq!(model["volumes"]["kestrel"]["name"], namespace.volume);
    assert_eq!(model["networks"]["link"]["name"], namespace.link);
    assert_eq!(
        model["services"]["kestrel"]["image"],
        namespace.control_plane
    );
    assert_eq!(
        model["services"]["kestrel-env"]["image"],
        namespace.environment
    );
    assert_eq!(
        model["services"]["kestrel"]["environment"]["KESTREL_NETWORK"],
        namespace.link
    );
    assert_eq!(
        model["services"]["kestrel"]["environment"]["KESTREL_IMAGE"],
        namespace.environment
    );
}

/// The namespace is derived, not random: one checkout gets the same names again, and a second
/// checkout on the same machine gets names that share none of its resources.
#[test]
fn the_suite_namespace_is_stable_for_one_checkout_and_distinct_for_another() {
    let a_checkout = compose::namespace_for(Path::new("/work/kestrel"));
    let the_same_checkout = compose::namespace_for(Path::new("/work/kestrel"));
    let another_checkout = compose::namespace_for(Path::new("/work/kestrel-elsewhere"));

    assert_eq!(the_same_checkout.project, a_checkout.project);
    assert_ne!(a_checkout.project, another_checkout.project);
    assert_ne!(a_checkout.volume, another_checkout.volume);
    assert_ne!(a_checkout.link, another_checkout.link);
    assert_ne!(a_checkout.control_plane, another_checkout.control_plane);
    assert_ne!(a_checkout.environment, another_checkout.environment);
}

#[test]
#[ignore = "builds the images the compose file names"]
fn the_stack_is_the_control_plane_the_filter_and_the_image_a_run_executes_in() {
    let namespace = compose::namespace_for(&docker::repository());
    let mut images = compose::built()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    images.sort_unstable();

    let [control_plane, environment, filter] = images[..] else {
        panic!("the compose file ships {images:?}, and it ships three images");
    };
    assert_eq!(control_plane, namespace.control_plane);
    assert_eq!(environment, namespace.environment);
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

/// Every request the driver makes goes through the filter, so a Run that reaches an Instance
/// and leaves no supervisor behind is the whole list a Run makes exercised. The Instance is
/// provisioned from this checkout's own image onto this checkout's own link network, so it
/// can neither find another checkout's control plane nor be found by it.
#[tokio::test]
#[ignore = "builds images and brings a stack up"]
async fn a_run_provisions_an_instance_and_stops_its_supervisor_through_the_filter() {
    let stack = Stack::up();
    let namespace = compose::namespace_for(&docker::repository());
    let workspace = a_workspace(&stack);
    let run = stack.ran(&["run", "enqueue", "--workspace", &workspace]);

    let instance = compose::until("the run to reach an instance", || {
        listed(&stack, &run).instance
    });
    let container = Container::named(&instance);
    assert_eq!(instance, format!("docker/kestrel-{run}"));
    assert!(
        container.networks().contains(&namespace.link),
        "the instance is on {}, not the checkout's link network {}",
        container.networks(),
        namespace.link
    );
    assert_eq!(container.image(), namespace.environment);

    // The link a supervisor dials is a container beside it rather than the host's gateway,
    // so reaching it at all is the network the control plane put it on.
    compose::until("the supervisor to reach the link", || {
        stack
            .everything_a_service_said("kestrel")
            .contains("link open")
            .then_some(())
    });

    // The credential this stack holds reaches no provider, so what ends this Run is the control
    // plane stopping under it rather than anything the agent did.
    stack.comes_back();

    let went = listed(&stack, &run).exit;
    assert_eq!(went["status"], "failed");
    assert_eq!(
        went["because"],
        "the control plane stopped while this run was in flight"
    );
    let left = container.processes();
    assert!(
        !left.contains("kestrel-supervisor"),
        "the run left its supervisor on its instance: {left}"
    );
    container.destroy();
}

#[test]
#[ignore = "builds images and brings a stack up"]
fn the_stack_comes_back_up_with_every_workspace_it_had() {
    let stack = Stack::up();
    let workspace = a_workspace(&stack);
    let shown = stack.ran(&["workspace", "show", &workspace, "--json", WORKSPACE]);
    let transcript = stack.ran(&["workspace", "transcript", &workspace, "--json", "seq,entry"]);

    stack.comes_back();

    assert_eq!(
        stack.ran(&["workspace", "show", &workspace, "--json", WORKSPACE]),
        shown
    );
    assert_eq!(
        stack.ran(&["workspace", "transcript", &workspace, "--json", "seq,entry"]),
        transcript
    );
    assert!(
        !transcript.is_empty(),
        "nothing was transcribed for the restart to keep"
    );
}

/// The commands `USAGE.md` walks a reader through, run by the installed Client against the
/// port the stack publishes, minus the two its neighbours already cover:
/// `a_run_provisions_an_instance_and_stops_its_supervisor_through_the_filter` covers enqueueing
/// a Run, and `the_stack_comes_back_up_with_every_workspace_it_had` covers surviving a restart.
#[test]
#[ignore = "builds images and brings a stack up"]
fn the_commands_usage_documents_are_the_commands_that_work() {
    let stack = Stack::up();
    let workspace = a_workspace(&stack);

    let opened = shown(&stack, &workspace);
    assert_eq!(opened["id"], workspace);
    assert_eq!(opened["organization"], "acme");
    assert_eq!(opened["project"], "kestrel");
    assert_eq!(opened["agent"], "builder");
    assert_eq!(opened["state"], "open");
    assert!(opened["opened_at"].is_string(), "{opened}");

    let transcript = stack.client(&["workspace", "transcript", "latest"]);
    assert!(
        transcript.out[0]
            .ends_with("\t{\"kind\":\"participant_joined\",\"participant\":\"builder\"}"),
        "USAGE.md shows the Agent joining as the first entry, and the transcript was:\n{:?}",
        transcript.out
    );
    assert!(
        transcript.err.contains(&format!("cursor  {workspace}:1")),
        "USAGE.md shows the cursor on stderr, and the transcript said:\n{}",
        transcript.err
    );

    stack.ran(&["workspace", "seal", "latest"]);

    assert_eq!(
        shown(&stack, &workspace)["state"],
        "sealed",
        "USAGE.md says sealing is visible on the Workspace, and it was not"
    );
}

const WORKSPACE: &str = "id,name,organization,project,agent,checkout,state,opened_at";

fn shown(stack: &Stack, workspace: &str) -> Value {
    let shown = stack.ran(&["workspace", "show", workspace, "--json", WORKSPACE]);

    serde_json::from_str(&shown).unwrap_or_else(|error| panic!("{shown} is no record: {error}"))
}

fn model(rendered: support::docker::Ran) -> Value {
    serde_json::from_str(&rendered.out)
        .unwrap_or_else(|error| panic!("docker compose config did not render JSON:\n{error}"))
}

fn a_workspace(stack: &Stack) -> String {
    stack.ran(&["organization", "declare", "acme"]);
    stack.ran(&[
        "project",
        "declare",
        "kestrel",
        "--repository",
        REPOSITORY,
        "--branch",
        "main",
    ]);
    // The Agent names no model, so the harness's own default is what a Run would get.
    stack.ran(&["agent", "declare", "builder"]);
    // A Run reaches no model without one. This value reaches no provider either, which is why
    // nothing here gets further than an Environment.
    stack.ran_given(
        &["credential", "set", "OPENCODE_API_KEY"],
        Some("not-a-key"),
    );

    stack.ran(&[
        "workspace",
        "open",
        "--project",
        "kestrel",
        "--agent",
        "builder",
    ])
}

struct Listed {
    instance: Option<String>,
    exit: Value,
}

fn listed(stack: &Stack, run: &str) -> Listed {
    let shown = stack.ran(&["run", "show", run, "--json", "instance,exit"]);
    let shown: Value =
        serde_json::from_str(&shown).unwrap_or_else(|error| panic!("{shown} is no run: {error}"));

    Listed {
        instance: shown["instance"].as_str().map(str::to_owned),
        exit: shown["exit"].clone(),
    }
}
