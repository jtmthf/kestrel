//! The Integration's outbound direction: a Run's exit status said back on the issue the work
//! came from. kestrel relays what its Agent said and reasons about no git of its own, so the
//! pull request a comment points at is there because the Agent named it.

use anyhow::Result;
use jiff::Timestamp;
use tracing::warn;

use crate::domain::{Direction, Exit, Integration, Outcome, Run, RunId, Session};
use crate::integration::back_off;
use crate::integration::github::{Github, Refused};
use crate::store::{Store, Tx};

/// Invisible where GitHub renders it, and the whole of how a delivery that never learned
/// whether its comment landed recognises its own.
fn marker(run: RunId) -> String {
    format!("<!-- kestrel run {run} -->")
}

pub(crate) async fn record(
    tx: &mut Tx<'_>,
    run: &Run,
    session: &Session,
    exit: &Exit,
) -> Result<()> {
    let Some(started_by) = session.started_by else {
        return Ok(());
    };

    let event = tx.integrations().event(started_by).await?;
    let integration = tx.integrations().with_id(event.integration).await?;
    if !integration.carries(Direction::Outbound) {
        warn!(
            integration = integration.name,
            run = %run.id,
            "this run's session came in through an integration that carries nothing outbound, \
             so its outcome reaches nobody"
        );
        return Ok(());
    }

    let said = tx.log().last_said(session).await?;
    let body = body(session, run, exit, said.as_deref());

    tx.integrations()
        .record_outcome(run, &integration, &event, &body)
        .await
}

/// One delivery attempt. What comes back is where the comment landed, or nothing — a refusal
/// defers the outcome rather than failing anything, because the Run's exit status is already
/// decided and nothing said afterwards changes it.
pub async fn deliver(store: &Store, github: &Github, outcome: &Outcome) -> Result<Option<String>> {
    let integration = {
        let mut tx = store.begin().await?;
        tx.integrations().with_id(outcome.integration).await?
    };

    // An earlier attempt went out and never came back, so a comment may already be there.
    if let Some(attempted_at) = outcome.attempted_at {
        match github
            .comment_carrying(
                &integration,
                outcome.subject,
                &marker(outcome.run),
                attempted_at,
            )
            .await
        {
            Ok(Some(already)) => return delivered(store, outcome, &already.html_url).await,
            Ok(None) => {}
            Err(refused) => return deferred(store, outcome, &integration, &refused).await,
        }
    }

    let mut tx = store.begin().await?;
    tx.integrations()
        .attempting_outcome(outcome, Timestamp::now())
        .await?;
    tx.commit().await?;

    match github
        .comment(&integration, outcome.subject, &outcome.body)
        .await
    {
        Ok(comment) => delivered(store, outcome, &comment.html_url).await,
        Err(refused) => deferred(store, outcome, &integration, &refused).await,
    }
}

async fn delivered(store: &Store, outcome: &Outcome, to: &str) -> Result<Option<String>> {
    let mut tx = store.begin().await?;
    tx.integrations().outcome_delivered(outcome, to).await?;
    tx.commit().await?;

    Ok(Some(to.to_owned()))
}

async fn deferred(
    store: &Store,
    outcome: &Outcome,
    integration: &Integration,
    refused: &Refused,
) -> Result<Option<String>> {
    warn!(
        run = %outcome.run,
        integration = integration.name,
        because = %refused,
        "a run's outcome could not be said back on the issue it came from"
    );

    let mut tx = store.begin().await?;
    tx.integrations()
        .outcome_deferred(outcome, back_off(integration, refused))
        .await?;
    tx.commit().await?;

    Ok(None)
}

fn body(session: &Session, run: &Run, exit: &Exit, said: Option<&str>) -> String {
    let mut body = format!("**kestrel** — run {exit}\n");

    if let Some(message) = said.map(str::trim).filter(|said| !said.is_empty()) {
        body.push('\n');
        for line in message.lines() {
            match line.trim().is_empty() {
                true => body.push_str(">\n"),
                false => body.push_str(&format!("> {line}\n")),
            }
        }
    }

    body.push_str(&format!(
        "\nSession `{}` · run `{}`\n{}\n",
        session.id,
        run.id,
        marker(run.id)
    ));

    body
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::domain::{
        Agent, AgentId, Organization, OrganizationId, RunState, SessionId, SessionState, Workspace,
        WorkspaceId,
    };

    fn a_session() -> Session {
        let organization = Organization {
            id: OrganizationId::generate(),
            name: "acme".to_owned(),
        };

        Session {
            id: SessionId::generate(),
            workspace: Workspace {
                id: WorkspaceId::generate(),
                organization: organization.id,
                name: "kestrel".to_owned(),
                repositories: Vec::new(),
                branch: "main".to_owned(),
            },
            agent: Agent {
                id: AgentId::generate(),
                organization: organization.id,
                name: "builder".to_owned(),
                runtime: "opencode".to_owned(),
                model: None,
            },
            organization,
            state: SessionState::Open,
            opened_at: Timestamp::now(),
            sealed_at: None,
            continues: None,
            started_by: None,
        }
    }

    fn a_run(session: &Session) -> Run {
        Run {
            id: RunId::generate(),
            organization: session.organization.id,
            session: session.id,
            state: RunState::Ended,
            exit: None,
            environment: None,
            model: None,
            enqueued_at: Timestamp::now(),
            started_at: None,
            ended_at: None,
            lease_expires_at: None,
            connected: None,
            usage: None,
        }
    }

    #[test]
    fn what_the_agent_said_last_is_quoted_under_the_exit_status() {
        let session = a_session();
        let run = a_run(&session);

        let body = body(
            &session,
            &run,
            &Exit::Succeeded,
            Some("Opened https://github.com/jtmthf/kestrel/pull/92.\n\nIt has a test."),
        );

        assert!(body.starts_with("**kestrel** — run succeeded\n"));
        assert!(body.contains(
            "> Opened https://github.com/jtmthf/kestrel/pull/92.\n>\n> It has a test.\n"
        ));
        assert!(body.contains(&session.id.to_string()));
        assert!(body.ends_with(&format!("{}\n", marker(run.id))));
    }

    #[test]
    fn a_run_that_failed_says_so_and_says_why() {
        let session = a_session();
        let run = a_run(&session);

        let body = body(
            &session,
            &run,
            &Exit::Failed {
                because: "the environment could not be provisioned".to_owned(),
            },
            None,
        );

        assert!(body.contains("run failed: the environment could not be provisioned"));
    }

    #[test]
    fn an_agent_that_said_nothing_leaves_no_empty_quote() {
        let session = a_session();
        let run = a_run(&session);

        let body = body(&session, &run, &Exit::Succeeded, Some("   "));

        assert!(!body.lines().any(|line| line.starts_with('>')));
    }
}
