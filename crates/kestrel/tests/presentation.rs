//! What the Client prints is decided by what is reading it: a person at a terminal, or a
//! script holding the other end of a pipe.

mod support;

use serde_json::Value;
use support::client::{self, Invocation, ran_on_a_terminal};
use support::{Kestrel, TOKEN};

const REPOSITORY: &str = "https://github.com/jtmthf/kestrel";

async fn an_organization_holding_two_agents() -> Kestrel {
    let kestrel = Kestrel::boot().await;
    let acme = kestrel.declare_organization("acme").await;
    kestrel
        .declare_project(&acme, "kestrel", &[REPOSITORY.to_owned()], "main")
        .await;
    kestrel
        .declare_agent(&acme, "builder", "opencode", Some("claude-opus-5"))
        .await;
    kestrel
        .declare_agent(&acme, "reviewer", "opencode", None)
        .await;

    kestrel
}

async fn piped(kestrel: &Kestrel, args: &[&str]) -> client::Finished {
    client::ran_by(kestrel, args, Invocation::default()).await
}

async fn on_a_terminal(kestrel: &Kestrel, args: &[&str], columns: u16) -> client::Shown {
    let operator = kestrel.operator();
    let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();

    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        ran_on_a_terminal(&operator, &args, columns, "")
    })
    .await
    .expect("the client should run")
}

#[tokio::test]
async fn a_pipe_gets_the_commands_own_fields_tab_delimited_without_a_flag() {
    let kestrel = an_organization_holding_two_agents().await;

    let listed = piped(&kestrel, &["agent", "list"]).await;

    let agents: Vec<Vec<&str>> = listed
        .out
        .iter()
        .map(|line| line.split('\t').collect())
        .collect();
    assert_eq!(agents.len(), 2, "{:?}", listed.out);
    assert_eq!(agents[0][1..], ["builder", "opencode", "claude-opus-5"]);
    assert_eq!(agents[1][1..], ["reviewer", "opencode", "-"]);
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_pipe_gets_no_header_to_strip_and_no_styling_to_strip_it_from() {
    let kestrel = an_organization_holding_two_agents().await;

    let listed = piped(&kestrel, &["agent", "list"]).await;

    let said = listed.out.join("\n");
    assert!(
        !said.contains("name\tharness"),
        "a pipe was given a header:\n{said}"
    );
    assert!(
        !said.contains('\u{1b}'),
        "a pipe was given styling:\n{said}"
    );
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_terminal_gets_the_records_under_the_fields_they_fill() {
    let kestrel = an_organization_holding_two_agents().await;

    let listed = on_a_terminal(&kestrel, &["agent", "list"], 120).await;

    assert!(listed.status.success(), "{}", listed.said);
    let [header, builder, reviewer] = listed.lines()[..] else {
        panic!("a terminal was shown {:?}", listed.lines());
    };
    assert!(header.starts_with("id "), "{header:?}");
    assert!(header.contains("name") && header.contains("harness") && header.contains("model"));
    assert_eq!(
        builder.find("opencode"),
        reviewer.find("opencode"),
        "the columns do not line up:\n{builder}\n{reviewer}"
    );
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_terminal_gets_a_detail_read_down_the_page_and_a_pipe_gets_it_across_one_line() {
    let kestrel = an_organization_holding_two_agents().await;
    let workspace = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let workspace = workspace.id.to_string();

    let watched = on_a_terminal(&kestrel, &["workspace", "show", &workspace], 120).await;
    let scripted = piped(&kestrel, &["workspace", "show", &workspace]).await;

    assert!(
        watched
            .lines()
            .iter()
            .any(|line| line.starts_with("id ") && line.ends_with(&workspace)),
        "a terminal was shown:\n{}",
        watched.said
    );
    assert!(
        watched
            .lines()
            .iter()
            .any(|line| line.starts_with("state ")),
        "a terminal was shown:\n{}",
        watched.said
    );
    assert_eq!(scripted.out.len(), 1, "{:?}", scripted.out);
    let fields: Vec<&str> = scripted.out[0].split('\t').collect();
    assert_eq!(fields[0], workspace);
    assert!(fields.contains(&"open"), "{fields:?}");
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_terminal_gets_lines_it_can_hold_and_a_pipe_gets_them_whole() {
    let kestrel = Kestrel::boot().await;
    let acme = kestrel.declare_organization("acme").await;
    let repositories = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"]
        .map(|name| format!("https://github.com/jtmthf/{name}-a-rather-long-repository-name"));
    kestrel
        .declare_project(&acme, "kestrel", &repositories, "main")
        .await;

    let watched = on_a_terminal(&kestrel, &["project", "list"], 60).await;
    let scripted = piped(&kestrel, &["project", "list"]).await;

    for line in watched.lines() {
        assert!(
            line.chars().count() <= 60,
            "{line:?} is wider than the terminal"
        );
    }
    for repository in &repositories {
        assert!(
            scripted.out[0].contains(repository.as_str()),
            "a pipe was given a truncated record:\n{}",
            scripted.out[0]
        );
    }
    kestrel.teardown().await;
}

#[tokio::test]
async fn json_emits_the_fields_it_names_and_nothing_the_boundary_adds() {
    let kestrel = an_organization_holding_two_agents().await;

    let listed = piped(&kestrel, &["agent", "list", "--json", "name,model"]).await;

    let agents: Vec<Value> = listed.records();
    assert_eq!(
        agents,
        [
            serde_json::json!({ "name": "builder", "model": "claude-opus-5" }),
            serde_json::json!({ "name": "reviewer", "model": null }),
        ]
    );
    kestrel.teardown().await;
}

#[tokio::test]
async fn json_without_a_field_list_is_refused_rather_than_guessed_at() {
    let kestrel = an_organization_holding_two_agents().await;

    let bare = piped(&kestrel, &["agent", "list", "--json"]).await;
    let empty = piped(&kestrel, &["agent", "list", "--json", ""]).await;
    let unknown = piped(&kestrel, &["agent", "list", "--json", "naem"]).await;

    for refused in [&bare, &empty, &unknown] {
        assert!(!refused.status.success(), "{:?}", refused.out);
        assert!(refused.out.is_empty(), "{:?}", refused.out);
    }
    assert!(bare.err.contains("--json"), "{}", bare.err);
    assert!(
        unknown.err.contains("no naem") && unknown.err.contains("name"),
        "{}",
        unknown.err
    );
    kestrel.teardown().await;
}

#[tokio::test]
async fn standard_output_carries_the_value_and_standard_error_carries_the_rest() {
    let kestrel = an_organization_holding_two_agents().await;
    let workspace = kestrel.open_workspace("acme", "kestrel", "builder").await;

    let read = piped(
        &kestrel,
        &["workspace", "transcript", &workspace.id.to_string()],
    )
    .await;
    let registered = piped(
        &kestrel,
        &[
            "integration",
            "register",
            "github",
            "hub",
            "--repository",
            "jtmthf/kestrel",
            "--token",
            TOKEN,
        ],
    )
    .await;

    assert!(
        read.out.iter().all(|line| !line.contains("cursor")),
        "the cursor was printed among the entries:\n{:?}",
        read.out
    );
    assert!(read.err.contains("cursor  "), "{}", read.err);
    assert_eq!(registered.out.len(), 1, "{:?}", registered.out);
    assert!(
        !registered.out[0].contains('\t'),
        "a creation answered more than the identifier it minted: {}",
        registered.out[0]
    );
    assert!(registered.err.is_empty(), "{}", registered.err);
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_pipe_on_standard_input_is_read_without_asking_for_anything() {
    let kestrel = an_organization_holding_two_agents().await;

    let held = client::ran_by(
        &kestrel,
        &["credential", "set", "ANTHROPIC_API_KEY"],
        Invocation::default().given("sk-a-key\n"),
    )
    .await;
    let nothing = client::ran_by(
        &kestrel,
        &["credential", "set", "OPENAI_API_KEY"],
        Invocation::default(),
    )
    .await;

    assert!(held.status.success(), "{}", held.err);
    assert!(
        held.err.is_empty(),
        "the client asked a pipe a question: {}",
        held.err
    );
    assert!(!nothing.status.success());
    assert!(
        !nothing.err.contains("ctrl-d"),
        "the client prompted a closed standard input: {}",
        nothing.err
    );
    kestrel.teardown().await;
}

#[tokio::test]
async fn a_terminal_on_standard_input_is_told_what_is_being_waited_for() {
    let kestrel = an_organization_holding_two_agents().await;
    let operator = kestrel.operator();

    let held = tokio::task::spawn_blocking(move || {
        ran_on_a_terminal(
            &operator,
            &["credential", "set", "ANTHROPIC_API_KEY"],
            80,
            "sk-a-key\n\u{4}",
        )
    })
    .await
    .expect("the client should run");

    assert!(held.status.success(), "{}", held.said);
    assert!(
        held.said.contains("a provider credential") && held.said.contains("standard input"),
        "a terminal was left waiting in silence:\n{}",
        held.said
    );
    kestrel.teardown().await;
}

/// A Transcript is the one listing whose records are prose, so the terminal presentation that
/// makes a table readable would make this command pointless.
#[tokio::test]
async fn a_transcript_reaches_a_terminal_whole_however_narrow_it_is() {
    let kestrel = an_organization_holding_two_agents().await;
    let workspace = kestrel.open_workspace("acme", "kestrel", "builder").await;
    let (run, _) = kestrel.dispatch_run(workspace.id).await;
    let said = "a line of what the agent had to say, and then\na second line after it";
    kestrel.said(&run, said).await;

    let read = on_a_terminal(
        &kestrel,
        &["workspace", "transcript", &workspace.id.to_string()],
        40,
    )
    .await;

    for line in said.lines() {
        assert!(
            read.said.contains(line),
            "the terminal was shown a clipped transcript:\n{}",
            read.said
        );
    }
    kestrel.teardown().await;
}
