use anyhow::{Result, bail};
use jiff::{SignedDuration, Timestamp};

use crate::domain::{
    Exit, Organization, Run, RunId, RunState, Workspace, WorkspaceId, WorkspaceState,
};
use crate::fanout::{self, Change};
use crate::instance;
use crate::log::{Cursor, Entry, Page, Unreadable, Window};
use crate::store::workspace::{Opening, Unfinished};
use crate::store::{Store, Tx};
use crate::work;

/// Generous, because kestrel has no signal that a human is watching a Workspace: duration is
/// standing in for presence.
const IDLE: SignedDuration = SignedDuration::from_hours(24);

pub async fn open(
    store: &Store,
    organization: &str,
    project: &str,
    agent: &str,
    profile: Option<&str>,
    branch: Option<&str>,
    continues: Option<&str>,
) -> Result<Workspace> {
    let mut tx = store.begin().await?;

    let organization = tx.organizations().named(organization).await?;
    let project = tx.projects().named(&organization, project).await?;
    let agent = tx.agents().named(&organization, agent).await?;
    let profile = match profile {
        Some(profile) => Some(tx.profiles().named(&organization, profile).await?),
        None => None,
    };
    let continues = match continues {
        Some(reference) => Some(continued(&mut tx, &organization, reference).await?),
        None => None,
    };

    let workspace = tx
        .workspaces()
        .open(Opening {
            organization: &organization,
            project: &project,
            agent: &agent,
            profile: profile.as_ref(),
            branch,
            correlation: None,
            continues: continues.as_ref(),
            started_by: None,
        })
        .await?;
    tx.log()
        .append(
            &workspace,
            Entry::ParticipantJoined {
                participant: workspace.agent.name.clone(),
            },
        )
        .await?;

    tx.commit().await?;
    fanout::publish(Change::WorkspaceOpened(&workspace));

    Ok(workspace)
}

pub async fn seal(store: &Store, id: WorkspaceId) -> Result<Workspace> {
    let mut tx = store.begin().await?;
    let workspace = tx.workspaces().get(id).await?;

    if workspace.state == WorkspaceState::Sealed {
        bail!("the workspace {id} is already sealed, and a sealed workspace is never reopened");
    }
    let unfinished = unfinished_run(&mut tx, &workspace).await?;
    if let Some(holding) = unfinished.in_flight() {
        bail!("the run {holding} is still in flight in the workspace {id}");
    }
    if let Some(waiting) = unfinished.waiting() {
        work::stopping(&mut tx, &waiting, Exit::Succeeded).await?;
    }
    instance::archive_on_seal(&mut tx, &workspace).await?;

    let sealed_at = tx.workspaces().seal(&workspace).await?;
    tx.commit().await?;

    let sealed = Workspace {
        state: WorkspaceState::Sealed,
        sealed_at: Some(sealed_at),
        ..workspace
    };
    fanout::publish(Change::WorkspaceSealed(&sealed));

    Ok(sealed)
}

/// Unattended sealing, through the same command a person seals with, so nothing here can
/// decide differently to `seal`.
pub async fn seal_idle(store: &Store) -> Result<Vec<Workspace>> {
    let mut sealed = Vec::new();

    for id in idle(store).await? {
        sealed.push(seal(store, id).await?);
    }

    Ok(sealed)
}

async fn idle(store: &Store) -> Result<Vec<WorkspaceId>> {
    let mut tx = store.begin().await?;
    let mut idle = Vec::new();

    for workspace in tx.workspaces().idle(Timestamp::now() - IDLE).await? {
        let holds_unpublished_work = match tx.workspaces().kept_instance(workspace.id).await? {
            Some(kept) => {
                instance::unpublished(&workspace.checkout.repositories, kept.observed.as_deref())
                    .is_some()
            }
            None => false,
        };
        if !holds_unpublished_work && unfinished_run(&mut tx, &workspace).await?.idle() {
            idle.push(workspace.id);
        }
    }

    Ok(idle)
}

pub(crate) struct UnfinishedRun {
    run: Option<Run>,
    held_input: bool,
}

pub(crate) async fn unfinished_run(
    tx: &mut Tx<'_>,
    workspace: &Workspace,
) -> Result<UnfinishedRun> {
    Ok(match tx.workspaces().unfinished_run(workspace).await? {
        Some(Unfinished { run, held_input }) => UnfinishedRun {
            run: Some(run),
            held_input,
        },
        None => UnfinishedRun {
            run: None,
            held_input: false,
        },
    })
}

pub(crate) enum PostDestination<'a> {
    Start,
    Brief,
    Held,
    Wake(&'a Run),
}

impl UnfinishedRun {
    /// A waiting Run is not in flight: sealing ends it (ADR-0024).
    pub fn in_flight(&self) -> Option<RunId> {
        self.run.as_ref().and_then(|run| {
            (!matches!(run.state, RunState::Ended | RunState::Waiting) || self.held_input)
                .then_some(run.id)
        })
    }

    pub fn waiting(&self) -> Option<Run> {
        self.run
            .as_ref()
            .filter(|run| run.state == RunState::Waiting)
            .cloned()
    }

    pub fn post_destination(&self) -> PostDestination<'_> {
        match &self.run {
            None => PostDestination::Start,
            Some(run) if run.state == RunState::Queued => PostDestination::Brief,
            Some(run) if run.state == RunState::Waiting => PostDestination::Wake(run),
            Some(_) => PostDestination::Held,
        }
    }

    pub fn refuses_enqueue(&self) -> Option<RunId> {
        self.run.as_ref().map(|run| run.id)
    }

    pub fn idle(&self) -> bool {
        self.in_flight().is_none()
    }
}

pub async fn show(store: &Store, id: WorkspaceId) -> Result<Workspace> {
    store.begin().await?.workspaces().get(id).await
}

pub async fn workspaces(store: &Store, organization: &str) -> Result<Vec<Workspace>> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.workspaces().all(&organization).await
}

pub async fn continuations(store: &Store, id: WorkspaceId) -> Result<Vec<WorkspaceId>> {
    store.begin().await?.workspaces().continuations(id).await
}

pub async fn post(
    store: &Store,
    id: WorkspaceId,
    participant: &str,
    message: &str,
) -> Result<Option<Run>> {
    let mut tx = store.begin().await?;
    let workspace = tx.workspaces().get(id).await?;
    let run = post_in(&mut tx, &workspace, participant, message).await?;
    tx.commit().await?;

    Ok(run)
}

pub(crate) async fn post_in(
    tx: &mut Tx<'_>,
    workspace: &Workspace,
    participant: &str,
    message: &str,
) -> Result<Option<Run>> {
    workspace.accepts("message")?;

    let unfinished = unfinished_run(tx, workspace).await?;
    match unfinished.post_destination() {
        PostDestination::Start => {
            said(tx, workspace, participant, message).await?;
            Ok(Some(tx.workspaces().enqueue_run(workspace, None).await?))
        }
        PostDestination::Brief => {
            said(tx, workspace, participant, message).await?;
            Ok(None)
        }
        PostDestination::Held => {
            tx.workspaces()
                .add_pending_message(workspace, participant, message)
                .await?;
            Ok(None)
        }
        // Held even for a waiting Run: its next turn waits for an active-work slot.
        PostDestination::Wake(waiting) => {
            tx.workspaces()
                .add_pending_message(workspace, participant, message)
                .await?;
            Ok(Some(waiting.clone()))
        }
    }
}

async fn said(
    tx: &mut Tx<'_>,
    workspace: &Workspace,
    participant: &str,
    message: &str,
) -> Result<()> {
    tx.log()
        .append(
            workspace,
            Entry::Said {
                participant: participant.to_owned(),
                message: message.to_owned(),
            },
        )
        .await?;

    Ok(())
}

pub async fn transcript(
    store: &Store,
    id: WorkspaceId,
    from: Option<Cursor>,
    window: Window,
) -> Result<Page, Unreadable> {
    let mut tx = store.begin().await?;
    let workspace = tx.workspaces().get(id).await?;

    tx.log().page(&workspace, from, window).await
}

pub async fn resolve(store: &Store, organization: &str, reference: &str) -> Result<Workspace> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.workspaces().resolved(&organization, reference).await
}

/// Only a sealed Workspace is continued: work an open one could still take belongs in it.
async fn continued(
    tx: &mut Tx<'_>,
    organization: &Organization,
    reference: &str,
) -> Result<Workspace> {
    let sealed = tx.workspaces().resolved(organization, reference).await?;

    if sealed.state != WorkspaceState::Sealed {
        bail!(
            "the workspace {} is open, and work continues in it rather than after it",
            sealed.id
        );
    }

    Ok(sealed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::OrganizationId;

    fn run(state: RunState) -> Run {
        Run {
            id: RunId::generate(),
            name: "run".into(),
            organization: OrganizationId::generate(),
            workspace: WorkspaceId::generate(),
            state,
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

    struct Case {
        state: RunState,
        held_input: bool,
        in_flight: bool,
        post: fn(&PostDestination) -> bool,
    }

    #[test]
    fn unfinished_run_rules_cover_every_phase_with_and_without_held_input() {
        use RunState::{Ended, Queued, Unreachable, Waiting, Working};
        let brief: fn(&PostDestination) -> bool = |post| matches!(post, PostDestination::Brief);
        let held: fn(&PostDestination) -> bool = |post| matches!(post, PostDestination::Held);
        let wake: fn(&PostDestination) -> bool = |post| matches!(post, PostDestination::Wake(_));
        #[rustfmt::skip]
        let cases = [
            Case { state: Queued,      held_input: false, in_flight: true,  post: brief },
            Case { state: Queued,      held_input: true,  in_flight: true,  post: brief },
            Case { state: Working,     held_input: false, in_flight: true,  post: held },
            Case { state: Working,     held_input: true,  in_flight: true,  post: held },
            Case { state: Waiting,     held_input: false, in_flight: false, post: wake },
            Case { state: Waiting,     held_input: true,  in_flight: true,  post: wake },
            Case { state: Ended,       held_input: false, in_flight: false, post: held },
            Case { state: Ended,       held_input: true,  in_flight: true,  post: held },
            Case { state: Unreachable, held_input: false, in_flight: true,  post: held },
            Case { state: Unreachable, held_input: true,  in_flight: true,  post: held },
        ];
        for case in cases {
            let run = run(case.state);
            let unfinished = UnfinishedRun {
                run: Some(run.clone()),
                held_input: case.held_input,
            };
            let label = format!("{} with held input {}", case.state, case.held_input);
            assert_eq!(unfinished.in_flight().is_some(), case.in_flight, "{label}");
            assert_eq!(unfinished.idle(), !case.in_flight, "{label}");
            assert!((case.post)(&unfinished.post_destination()), "{label}");
            assert_eq!(unfinished.refuses_enqueue(), Some(run.id), "{label}");
            assert_eq!(
                unfinished.waiting().is_some(),
                case.state == Waiting,
                "{label}"
            );
        }
        let empty = UnfinishedRun {
            run: None,
            held_input: false,
        };
        assert!(matches!(empty.post_destination(), PostDestination::Start));
        assert!(empty.idle());
        assert_eq!(empty.refuses_enqueue(), None);
    }
}
