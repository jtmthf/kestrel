mod support;

use serde_json::Value;
use support::Kestrel;
use support::client::{Finished, Invocation, ran_by};

const RESOLVED: &str = "control_plane,control_plane_source,organization,organization_source,\
                        projects,agents,triggers,workspaces,integrations,credentials,profiles,next";
const UNRESOLVED: &str = "control_plane,organization,organization_source,organizations,next";

fn names(records: &[Value]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record["name"].as_str().expect("a name"))
        .collect()
}

fn bound_to(organization: &str) -> Invocation {
    Invocation::default().file(".kestrel/organization", organization)
}

fn refused(finished: &Finished, naming: &[&str]) {
    assert!(!finished.status.success(), "the client did not refuse");
    for named in naming {
        assert!(
            finished.err.contains(named),
            "the refusal does not name {named}:\n{}",
            finished.err
        );
    }
}

/// Two Organizations, each holding a Project a listing can tell apart.
async fn two_organizations() -> Kestrel {
    let kestrel = Kestrel::boot().await;
    let acme = kestrel.declare_organization("acme").await;
    let globex = kestrel.declare_organization("globex").await;
    let repository = vec!["https://github.com/jtmthf/kestrel".to_owned()];
    kestrel
        .declare_project(&acme, "for-acme", &repository, "main")
        .await;
    kestrel
        .declare_project(&globex, "for-globex", &repository, "main")
        .await;

    kestrel
}

#[tokio::test]
async fn the_flag_names_the_scope_ahead_of_the_environment_and_a_binding() {
    let kestrel = two_organizations().await;

    let listed = ran_by(
        &kestrel,
        &[
            "project",
            "list",
            "--json",
            "name",
            "--organization",
            "acme",
        ],
        bound_to("globex").env("KESTREL_ORGANIZATION", "globex"),
    )
    .await;

    assert_eq!(names(&listed.records()), ["for-acme"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn the_environment_names_the_scope_ahead_of_a_binding() {
    let kestrel = two_organizations().await;

    let listed = ran_by(
        &kestrel,
        &["project", "list", "--json", "name"],
        bound_to("acme").env("KESTREL_ORGANIZATION", "globex"),
    )
    .await;

    assert_eq!(names(&listed.records()), ["for-globex"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_committed_binding_names_the_scope_from_the_working_directory() {
    let kestrel = two_organizations().await;

    let listed = ran_by(
        &kestrel,
        &["project", "list", "--json", "name"],
        bound_to("acme"),
    )
    .await;

    assert_eq!(names(&listed.records()), ["for-acme"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_committed_binding_names_the_scope_from_anywhere_in_its_repository() {
    let kestrel = two_organizations().await;

    let listed = ran_by(
        &kestrel,
        &["project", "list", "--json", "name"],
        bound_to("acme")
            .file(".git/HEAD", "ref: refs/heads/main\n")
            .within("crates/kestrel"),
    )
    .await;

    assert_eq!(names(&listed.records()), ["for-acme"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_binding_above_the_repository_is_no_remembered_scope() {
    let kestrel = two_organizations().await;

    let listed = ran_by(
        &kestrel,
        &["project", "list"],
        bound_to("acme")
            .file("kestrel/.git/HEAD", "ref: refs/heads/main\n")
            .within("kestrel/crates"),
    )
    .await;

    refused(&listed, &["--organization", "acme", "globex"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn the_only_organization_is_the_scope_when_nothing_names_one() {
    let kestrel = Kestrel::boot().await;
    let acme = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(
            &acme,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;

    let listed = ran_by(
        &kestrel,
        &["project", "list", "--json", "name"],
        Invocation::default(),
    )
    .await;

    assert_eq!(names(&listed.records()), ["kestrel"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn two_organizations_make_an_unqualified_command_fail_before_writing_anything() {
    let kestrel = two_organizations().await;

    let declared = ran_by(
        &kestrel,
        &[
            "project",
            "declare",
            "kestrel",
            "--repository",
            "https://github.com/jtmthf/kestrel",
            "--branch",
            "main",
        ],
        Invocation::default(),
    )
    .await;

    refused(&declared, &["--organization", "acme", "globex"]);
    let mut held = Vec::new();
    for organization in kestrel.organizations().await {
        held.extend(
            kestrel
                .projects(&organization)
                .await
                .into_iter()
                .map(|project| project.name),
        );
    }
    held.sort();
    assert_eq!(held, ["for-acme", "for-globex"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn every_scoped_listing_refuses_to_guess_between_two_organizations() {
    let kestrel = two_organizations().await;

    for noun in [
        "project",
        "agent",
        "credential",
        "profile",
        "integration",
        "event",
        "trigger",
        "workspace",
    ] {
        let listed = ran_by(&kestrel, &[noun, "list"], Invocation::default()).await;
        refused(&listed, &["--organization", "acme", "globex"]);
    }
    kestrel.teardown().await;
}

#[tokio::test]
async fn no_organization_at_all_names_the_command_that_makes_one() {
    let kestrel = Kestrel::boot().await;

    let listed = ran_by(&kestrel, &["project", "list"], Invocation::default()).await;

    refused(&listed, &["organization declare"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn an_empty_name_is_refused_rather_than_falling_through_to_another_scope() {
    let kestrel = Kestrel::boot().await;
    kestrel.declare_organization("acme").await;

    let flagged = ran_by(
        &kestrel,
        &["project", "list", "--organization", ""],
        bound_to("acme").env("KESTREL_ORGANIZATION", "acme"),
    )
    .await;
    let exported = ran_by(
        &kestrel,
        &["project", "list"],
        bound_to("acme").env("KESTREL_ORGANIZATION", ""),
    )
    .await;

    refused(&flagged, &["--organization"]);
    refused(&exported, &["--organization"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn status_prints_every_resolved_value_its_source_what_exists_and_what_to_run_next() {
    let kestrel = Kestrel::boot().await;
    let acme = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(
            &acme,
            "kestrel",
            &["https://github.com/jtmthf/kestrel".to_owned()],
            "main",
        )
        .await;
    kestrel
        .declare_agent(&acme, "builder", "opencode", None)
        .await;

    let reported = ran_by(
        &kestrel,
        &["status", "--organization", "acme", "--json", RESOLVED],
        Invocation::default(),
    )
    .await
    .records();

    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0]["control_plane"], kestrel.operator());
    assert_eq!(reported[0]["control_plane_source"], "KESTREL_CONTROL_PLANE");
    assert_eq!(reported[0]["organization"], "acme");
    assert_eq!(reported[0]["organization_source"], "--organization");
    assert_eq!(reported[0]["projects"], 1);
    assert_eq!(reported[0]["agents"], 1);
    assert_eq!(reported[0]["triggers"], 0);
    assert_eq!(reported[0]["workspaces"], 0);
    assert_eq!(reported[0]["integrations"], 0);
    assert_eq!(reported[0]["credentials"], 0);
    assert_eq!(reported[0]["profiles"], 0);
    assert_eq!(
        reported[0]["next"],
        "kestrel workspace open --project kestrel --agent builder"
    );
    kestrel.teardown().await;
}

#[tokio::test]
async fn status_names_the_environment_and_the_binding_when_each_is_the_source() {
    let kestrel = Kestrel::boot().await;
    kestrel.declare_organization("acme").await;

    let sourced = &["status", "--json", "organization_source"];
    let environment = ran_by(
        &kestrel,
        sourced,
        Invocation::default().env("KESTREL_ORGANIZATION", "acme"),
    )
    .await
    .records();
    let binding = ran_by(&kestrel, sourced, bound_to("acme")).await.records();
    let only = ran_by(&kestrel, sourced, Invocation::default())
        .await
        .records();

    assert_eq!(
        environment[0]["organization_source"],
        "KESTREL_ORGANIZATION"
    );
    assert!(
        binding[0]["organization_source"]
            .as_str()
            .expect("a bound path")
            .ends_with(".kestrel/organization"),
        "{}",
        binding[0]["organization_source"]
    );
    assert_eq!(only[0]["organization_source"], "only organization");
    kestrel.teardown().await;
}

#[tokio::test]
async fn status_explains_an_unresolved_scope_instead_of_failing() {
    let kestrel = two_organizations().await;

    let reported = ran_by(
        &kestrel,
        &["status", "--json", UNRESOLVED],
        Invocation::default(),
    )
    .await
    .records();

    assert_eq!(reported[0]["control_plane"], kestrel.operator());
    assert_eq!(reported[0]["organization"], Value::Null);
    assert_eq!(reported[0]["organization_source"], Value::Null);
    assert_eq!(
        reported[0]["organizations"],
        serde_json::json!(["acme", "globex"])
    );
    assert_eq!(reported[0]["next"], "kestrel status --organization <name>");
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_command_naming_its_record_needs_no_scope() {
    let kestrel = two_organizations().await;

    let event = ran_by(
        &kestrel,
        &["event", "show", "yesterday"],
        Invocation::default(),
    )
    .await;

    refused(&event, &["no event yesterday"]);
    kestrel.teardown().await;
}

/// A Workspace reference is resolved inside the scope the invocation named, so it cannot be
/// reached without one, unlike an Event's record which stands alone.
#[tokio::test]
async fn a_workspace_reference_refuses_to_guess_between_two_organizations() {
    let kestrel = two_organizations().await;

    let shown = ran_by(
        &kestrel,
        &["workspace", "show", "01a0a2d8-baf8-7c02-99fa-7280f174c14a"],
        Invocation::default(),
    )
    .await;

    refused(&shown, &["--organization", "acme", "globex"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_command_naming_its_record_refuses_a_flag_it_would_ignore() {
    let kestrel = two_organizations().await;

    let flagged = ran_by(
        &kestrel,
        &["event", "show", "yesterday", "--organization", "globex"],
        Invocation::default(),
    )
    .await;
    let exported = ran_by(
        &kestrel,
        &["event", "show", "yesterday"],
        Invocation::default().env("KESTREL_ORGANIZATION", "globex"),
    )
    .await;

    refused(&flagged, &["--organization scopes nothing"]);
    refused(&exported, &["no event yesterday"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn the_client_keeps_no_current_context_and_switches_none() {
    let kestrel = Kestrel::boot().await;
    let acme = kestrel.declare_organization("acme").await;

    let switched = ran_by(&kestrel, &["context", "use", "acme"], Invocation::default()).await;
    let configured = ran_by(
        &kestrel,
        &["config", "use-context", "acme"],
        Invocation::default(),
    )
    .await;
    let status = ran_by(
        &kestrel,
        &["status", "--organization", "acme", "--json", "organization"],
        Invocation::default(),
    )
    .await;

    assert!(!switched.status.success());
    assert!(!configured.status.success());
    assert_eq!(status.records()[0]["organization"], "acme");
    assert_eq!(
        kestrel.organizations().await[0].id,
        acme.id,
        "a scope-switch would have rewritten the Organization"
    );
    assert!(
        status.left_behind.is_empty(),
        "the client persisted a current context in {:?}",
        status.left_behind
    );
    kestrel.teardown().await;
}
