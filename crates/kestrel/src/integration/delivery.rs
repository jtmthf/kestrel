//! The Integration's outbound direction: a completed Turn's response, and a Run's own final
//! Outcome when it adds something the Turn responses did not, said back where the work came
//! from (ADR-0024). kestrel relays what its Agent said and reasons about no git of its own, so
//! the pull request a comment points at is there because the Agent named it.

use anyhow::Result;
use jiff::Timestamp;
use tracing::warn;

use crate::domain::{Delivery, Direction, Event, Exit, Integration, Run, RunId, Session};
use crate::integration::back_off;
use crate::integration::github::{Github, MARKER, Refused};
use crate::store::{Store, Tx};

/// Invisible where GitHub renders it, and the whole of how a delivery that never learned
/// whether its comment landed recognises its own. The Turn names which of a Run's messages it is.
fn marker(run: RunId, turn: Option<i64>) -> String {
    match turn {
        Some(turn) => format!("{MARKER}{run} turn {turn} -->"),
        None => format!("{MARKER}{run} -->"),
    }
}

/// The surface a Session came in through, when it came in through one that carries outbound.
async fn surface(tx: &mut Tx<'_>, session: &Session) -> Result<Option<(Integration, Event)>> {
    let Some(started_by) = session.started_by else {
        return Ok(None);
    };

    let event = tx.integrations().event(started_by).await?;
    let Some(integration) = event.integration else {
        return Ok(None);
    };
    let integration = tx.integrations().with_id(integration).await?;
    if !integration.carries(Direction::Outbound) {
        warn!(
            integration = integration.name,
            "a session came in through an integration that carries nothing outbound, so what it \
             says reaches nobody"
        );
        return Ok(None);
    }

    Ok(Some((integration, event)))
}

/// Recorded in the transaction that answers a Turn, so a Turn once over has a response waiting
/// to be posted. A Turn that produced no message has nothing to say and records nothing.
pub(crate) async fn record_turn(
    tx: &mut Tx<'_>,
    run: &Run,
    session: &Session,
    turn: i64,
    said: &[String],
) -> Result<()> {
    let Some((integration, event)) = surface(tx, session).await? else {
        return Ok(());
    };

    let body = format!("{}\n\n{}\n", said.join("\n\n"), marker(run.id, Some(turn)));
    tx.integrations()
        .record_delivery(run, &integration, &event, Some(turn), &body, Some(said))
        .await
}

pub(crate) async fn record_outcome(
    tx: &mut Tx<'_>,
    run: &Run,
    session: &Session,
    exit: &Exit,
    said: Option<&str>,
) -> Result<()> {
    if matches!(exit, Exit::Succeeded) {
        let responses = tx.integrations().turn_responses(run.id).await?;
        if !responses.is_empty()
            && said.is_none_or(|said| {
                responses.iter().any(|messages| {
                    messages.iter().any(|message| message == said) || messages.join("\n\n") == said
                })
            })
        {
            return Ok(());
        }
    }
    let Some((integration, event)) = surface(tx, session).await? else {
        return Ok(());
    };

    let body = body(session, run, exit, said);

    tx.integrations()
        .record_delivery(run, &integration, &event, None, &body, None)
        .await
}

/// One delivery attempt. What comes back is where the comment landed, or nothing — a refusal
/// defers it rather than failing anything, because the Turn is already over and nothing said
/// afterwards changes it.
pub async fn deliver(
    store: &Store,
    github: &Github,
    delivery: &Delivery,
) -> Result<Option<String>> {
    let integration = {
        let mut tx = store.begin().await?;
        tx.integrations().with_id(delivery.integration).await?
    };
    let marker = marker(delivery.run, delivery.turn);

    // An earlier attempt went out and never came back, so a comment may already be there.
    if let Some(attempted_at) = delivery.attempted_at {
        match github
            .comment_carrying(&integration, delivery.subject, &marker, attempted_at)
            .await
        {
            Ok(Some(already)) => return delivered(store, delivery, &already.html_url).await,
            Ok(None) => {}
            Err(refused) => return deferred(store, delivery, &integration, &refused).await,
        }
    }

    let mut tx = store.begin().await?;
    tx.integrations()
        .attempting_delivery(delivery, Timestamp::now())
        .await?;
    tx.commit().await?;

    match github
        .comment(&integration, delivery.subject, &delivery.body)
        .await
    {
        Ok(comment) => delivered(store, delivery, &comment.html_url).await,
        Err(refused) => deferred(store, delivery, &integration, &refused).await,
    }
}

async fn delivered(store: &Store, delivery: &Delivery, to: &str) -> Result<Option<String>> {
    let mut tx = store.begin().await?;
    tx.integrations().delivery_delivered(delivery, to).await?;
    tx.commit().await?;

    Ok(Some(to.to_owned()))
}

async fn deferred(
    store: &Store,
    delivery: &Delivery,
    integration: &Integration,
    refused: &Refused,
) -> Result<Option<String>> {
    warn!(
        run = %delivery.run,
        turn = delivery.turn,
        integration = integration.name,
        because = %refused,
        "what a run said could not be said back on the issue it came from"
    );

    let mut tx = store.begin().await?;
    tx.integrations()
        .delivery_deferred(delivery, back_off(integration.github()?.interval, refused))
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
        marker(run.id, None)
    ));

    body
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::domain::{
        Agent, AgentId, Checkout, Organization, OrganizationId, RunState, SessionId, SessionState,
        Workspace, WorkspaceId,
    };

    fn a_session() -> Session {
        let organization = Organization {
            id: OrganizationId::generate(),
            name: "acme".to_owned(),
            max_live_instances: None,
        };

        Session {
            id: SessionId::generate(),
            name: "bright-falcon".to_owned(),
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
            profile: None,
            organization,
            checkout: Checkout {
                repositories: Vec::new(),
                base: "main".to_owned(),
                branch: "main".to_owned(),
            },
            correlation: None,
            state: SessionState::Open,
            opened_at: Timestamp::now(),
            last_active_at: Timestamp::now(),
            sealed_at: None,
            continues: None,
            started_by: None,
        }
    }

    fn a_run(session: &Session) -> Run {
        Run {
            id: RunId::generate(),
            name: "quiet-river".to_owned(),
            organization: session.organization.id,
            session: session.id,
            state: RunState::Ended,
            waiting_for: None,
            exit: None,
            outcome_message: None,
            instance: None,
            supervisor: None,
            model: None,
            worked_model: None,
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
        assert!(body.ends_with(&format!("{}\n", marker(run.id, None))));
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

    #[test]
    fn a_turns_marker_names_the_run_and_the_turn() {
        let run = RunId::generate();

        assert_eq!(marker(run, Some(3)), format!("{MARKER}{run} turn 3 -->"));
        assert_eq!(marker(run, None), format!("{MARKER}{run} -->"));
        assert_ne!(marker(run, Some(1)), marker(run, Some(2)));
    }
}
