use anyhow::Result;
use tracing::info;

use crate::domain::{Event, EventRecordId, RunId, Workspace, WorkspaceId, WorkspaceState};
use crate::filter::Author;
use crate::integration::github;
use crate::store::{Store, Tx};

const AT_A_TIME: usize = 32;

pub struct Received {
    pub event: EventRecordId,
    pub workspace: WorkspaceId,
    pub run: Option<RunId>,
}

pub async fn receive(store: &Store) -> Result<Vec<Received>> {
    let events = {
        let mut tx = store.begin().await?;
        tx.integrations()
            .unfollowed(github::COMMENTED, AT_A_TIME)
            .await?
    };

    let mut received = Vec::with_capacity(events.len());
    for event in events {
        received.push(receiving(store, &event).await?);
    }

    Ok(received)
}

/// A comment only feeds open work: a command belongs to the Triggers, and only one continues a
/// sealed Workspace.
async fn receiving(store: &Store, event: &Event) -> Result<Received> {
    let mut tx = store.begin().await?;
    let mut workspace = tx
        .integrations()
        .workspace_for_follow_up(event)
        .await?
        .expect("an unfollowed event has an originating workspace");

    let holding = match (&workspace.state, &workspace.correlation) {
        (WorkspaceState::Sealed, Some(correlation)) => {
            tx.workspaces()
                .holding_correlation(&workspace.organization, correlation)
                .await?
        }
        _ => None,
    };
    if let Some(holding) = holding {
        let open = tx.workspaces().get(holding).await?;
        let after_opening_event = match open.started_by {
            Some(origin) => {
                let origin = tx.integrations().event(origin).await?;
                github::at_or_after(&event.occurrence, &origin.occurrence)
            }
            None => true,
        };
        if after_opening_event {
            workspace = open;
        }
    }

    let data = github::EventData::new(&event.occurrence);
    // A command belongs to the Triggers, which is where whether it may start or feed work is
    // decided; only a remark is judged here.
    let command = data.command().is_some();
    let feeds = workspace.state != WorkspaceState::Sealed
        && !command
        && admitted(&mut tx, &workspace, event).await?;
    if workspace.state != WorkspaceState::Sealed && !command && !feeds {
        info!(
            workspace = %workspace.id,
            author = data.actor().unwrap_or_default(),
            "a comment from an author the workspace's trigger does not authorize was not taken as input"
        );
    }
    let run = if feeds {
        crate::workspace::post_in(
            &mut tx,
            &workspace,
            data.actor().unwrap_or_default(),
            data.message().unwrap_or_default(),
        )
        .await?
    } else {
        None
    };
    tx.integrations()
        .record_follow_up(event, &workspace)
        .await?;
    tx.commit().await?;

    Ok(Received {
        event: event.record_id,
        workspace: workspace.id,
        run: run.map(|run| run.id),
    })
}

/// A remark feeds an open Workspace only from someone the Trigger that opened it authorizes.
/// Whether a command may start work is the Trigger's filter to say, and that path never gets
/// here, so this is about input to work already open.
async fn admitted(tx: &mut Tx<'_>, workspace: &Workspace, event: &Event) -> Result<bool> {
    let Some(trigger) = tx.triggers().opening_of(workspace.id).await? else {
        return Ok(true);
    };
    let data = github::EventData::new(&event.occurrence);

    Ok(trigger.filter().admits(Author {
        login: data.actor(),
        association: data.association(),
    }))
}
