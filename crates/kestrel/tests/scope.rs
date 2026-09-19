mod support;

use serde_json::Value;
use support::Harness;
use support::client::{self, Finished};

fn records(finished: &Finished) -> Vec<Value> {
    assert!(
        finished.status.success(),
        "the client failed:\n{}",
        finished.err
    );
    finished
        .out
        .iter()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{line} is not a record: {error}"))
        })
        .collect()
}

fn names(records: &[Value]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record["name"].as_str().expect("a name"))
        .collect()
}

async fn ran(
    harness: &Harness,
    args: &[&str],
    environment: &[(&str, &str)],
    binding: Option<&str>,
) -> Finished {
    let operator = harness.operator();
    let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();
    let environment: Vec<(String, String)> = environment
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    let binding = binding.map(str::to_owned);

    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let environment: Vec<(&str, &str)> = environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        client::ran_configured(&operator, &args, &environment, binding.as_deref())
    })
    .await
    .expect("the client should run")
}

/// Two Organizations, each holding a Workspace a listing can tell apart.
async fn two_organizations() -> Harness {
    let harness = Harness::boot().await;
    let acme = harness.declare_organization("acme").await;
    let globex = harness.declare_organization("globex").await;
    let repository = vec!["https://github.com/jtmthf/kestrel".to_owned()];
    harness
        .declare_workspace(&acme, "for-acme", &repository, "main")
        .await;
    harness
        .declare_workspace(&globex, "for-globex", &repository, "main")
        .await;

    harness
}

#[tokio::test]
async fn the_flag_names_the_scope_ahead_of_the_environment_and_a_binding() {
    let harness = two_organizations().await;

    let listed = ran(
        &harness,
        &["workspace", "list", "--organization", "acme"],
        &[("KESTREL_ORGANIZATION", "globex")],
        Some("globex"),
    )
    .await;

    assert_eq!(names(&records(&listed)), ["for-acme"]);
    harness.teardown().await;
}

#[tokio::test]
async fn the_environment_names_the_scope_ahead_of_a_binding() {
    let harness = two_organizations().await;

    let listed = ran(
        &harness,
        &["workspace", "list"],
        &[("KESTREL_ORGANIZATION", "globex")],
        Some("acme"),
    )
    .await;

    assert_eq!(names(&records(&listed)), ["for-globex"]);
    harness.teardown().await;
}

#[tokio::test]
async fn a_committed_binding_names_the_scope_from_the_working_directory() {
    let harness = two_organizations().await;

    let listed = ran(&harness, &["workspace", "list"], &[], Some("acme")).await;

    assert_eq!(names(&records(&listed)), ["for-acme"]);
    harness.teardown().await;
}

#[tokio::test]
async fn the_only_organization_is_the_scope_when_nothing_names_one() {
    let harness = Harness::boot().await;
    let acme = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &acme,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;

    let listed = ran(&harness, &["workspace", "list"], &[], None).await;

    assert_eq!(names(&records(&listed)), ["kestrel"]);
    harness.teardown().await;
}

#[tokio::test]
async fn two_organizations_make_an_unqualified_command_fail_before_writing_anything() {
    let harness = two_organizations().await;

    let refused = ran(
        &harness,
        &[
            "workspace",
            "declare",
            "kestrel",
            "--repository",
            "https://github.com/jtmthf/kestrel",
            "--branch",
            "main",
        ],
        &[],
        None,
    )
    .await;

    assert!(!refused.status.success());
    assert!(
        refused.err.contains("--organization"),
        "the refusal does not say what would fix it:\n{}",
        refused.err
    );
    assert!(
        refused.err.contains("acme") && refused.err.contains("globex"),
        "the refusal does not name what exists:\n{}",
        refused.err
    );
    let mut held = Vec::new();
    for organization in harness.organizations().await {
        held.extend(
            harness
                .workspaces(&organization)
                .await
                .into_iter()
                .map(|workspace| workspace.name),
        );
    }
    held.sort();
    assert_eq!(held, ["for-acme", "for-globex"]);
    harness.teardown().await;
}

#[tokio::test]
async fn no_organization_at_all_names_the_command_that_makes_one() {
    let harness = Harness::boot().await;

    let refused = ran(&harness, &["workspace", "list"], &[], None).await;

    assert!(!refused.status.success());
    assert!(
        refused.err.contains("organization declare"),
        "the refusal does not name the command that fixes it:\n{}",
        refused.err
    );
    harness.teardown().await;
}

#[tokio::test]
async fn an_empty_flag_is_refused_rather_than_falling_through_to_another_scope() {
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;

    let refused = ran(
        &harness,
        &["workspace", "list", "--organization", ""],
        &[("KESTREL_ORGANIZATION", "acme")],
        Some("acme"),
    )
    .await;

    assert!(!refused.status.success());
    assert!(
        refused.err.contains("organization"),
        "the refusal does not name what is empty:\n{}",
        refused.err
    );
    harness.teardown().await;
}

#[tokio::test]
async fn status_prints_every_resolved_value_its_source_what_exists_and_what_to_run_next() {
    let harness = Harness::boot().await;
    let acme = harness.declare_organization("acme").await;
    harness
        .declare_workspace(
            &acme,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    harness
        .declare_agent(&acme, "builder", "opencode", None)
        .await;

    let reported = records(&ran(&harness, &["status", "--organization", "acme"], &[], None).await);

    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0]["control_plane"], harness.operator());
    assert_eq!(reported[0]["control_plane_source"], "KESTREL_CONTROL_PLANE");
    assert_eq!(reported[0]["organization"], "acme");
    assert_eq!(reported[0]["organization_source"], "--organization");
    assert_eq!(reported[0]["binding"], Value::Null);
    assert_eq!(reported[0]["workspaces"], 1);
    assert_eq!(reported[0]["agents"], 1);
    assert_eq!(reported[0]["triggers"], 0);
    assert_eq!(reported[0]["sessions"], 0);
    assert_eq!(reported[0]["integrations"], 0);
    assert_eq!(reported[0]["credentials"], 0);
    assert_eq!(reported[0]["profiles"], 0);
    assert_eq!(
        reported[0]["next"],
        "kestrel-client session open --workspace kestrel --agent builder"
    );
    harness.teardown().await;
}

#[tokio::test]
async fn status_names_the_environment_and_the_binding_when_each_is_the_source() {
    let harness = Harness::boot().await;
    harness.declare_organization("acme").await;

    let environment = records(
        &ran(
            &harness,
            &["status"],
            &[("KESTREL_ORGANIZATION", "acme")],
            None,
        )
        .await,
    );
    let binding = records(&ran(&harness, &["status"], &[], Some("acme")).await);
    let only = records(&ran(&harness, &["status"], &[], None).await);

    assert_eq!(
        environment[0]["organization_source"],
        "KESTREL_ORGANIZATION"
    );
    assert_eq!(
        binding[0]["organization_source"], binding[0]["binding"],
        "the source of a bound scope is the binding it was read from"
    );
    assert!(
        binding[0]["binding"]
            .as_str()
            .expect("a bound path")
            .ends_with(".kestrel/organization"),
        "{}",
        binding[0]["binding"]
    );
    assert_eq!(only[0]["organization_source"], "only organization");
    harness.teardown().await;
}

#[tokio::test]
async fn an_unscoped_read_still_addresses_the_control_plane_without_a_scope() {
    let harness = two_organizations().await;

    // Nothing resolves scope for a command that names a record directly, so two Organizations
    // are not ambiguous where there is nothing to scope.
    let shown = ran(
        &harness,
        &["session", "show", "01a0a2d8-baf8-7c02-99fa-7280f174c14a"],
        &[],
        None,
    )
    .await;
    let event = ran(&harness, &["event", "show", "yesterday"], &[], None).await;

    assert!(!shown.status.success());
    assert!(shown.err.contains("no session"), "{}", shown.err);
    assert!(!event.status.success());
    assert!(event.err.contains("no event yesterday"), "{}", event.err);
    harness.teardown().await;
}

#[tokio::test]
async fn the_client_keeps_no_current_context_and_switches_none() {
    let harness = Harness::boot().await;
    let acme = harness.declare_organization("acme").await;

    let switched = ran(&harness, &["context", "use", "acme"], &[], None).await;
    let configured = ran(&harness, &["config", "use-context", "acme"], &[], None).await;
    let status = ran(&harness, &["status", "--organization", "acme"], &[], None).await;
    let reported = records(&status);

    assert!(!switched.status.success());
    assert!(!configured.status.success());
    assert_eq!(reported[0]["organization"], "acme");
    assert_eq!(
        harness.organizations().await[0].id,
        acme.id,
        "a scope-switch would have rewritten the Organization"
    );
    assert!(
        status.left_behind.is_empty(),
        "the client persisted a current context in {:?}",
        status.left_behind
    );
    harness.teardown().await;
}
