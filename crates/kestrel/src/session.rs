use anyhow::{Result, bail};
use jiff::{SignedDuration, Timestamp};

use crate::domain::{Exit, Organization, Run, RunId, RunState, Session, SessionId, SessionState};
use crate::fanout::{self, Change};
use crate::instance;
use crate::log::{Cursor, Entry, Page, Unreadable, Window};
use crate::store::session::{Opening, Unfinished};
use crate::store::{Store, Tx};
use crate::work;

/// Generous, because kestrel has no signal that a human is watching a Session: duration is
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
) -> Result<Session> {
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

    let session = tx
        .sessions()
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
            &session,
            Entry::ParticipantJoined {
                participant: session.agent.name.clone(),
            },
        )
        .await?;

    tx.commit().await?;
    fanout::publish(Change::SessionOpened(&session));

    Ok(session)
}

pub async fn seal(store: &Store, id: SessionId) -> Result<Session> {
    let mut tx = store.begin().await?;
    let session = tx.sessions().get(id).await?;

    if session.state == SessionState::Sealed {
        bail!("the session {id} is already sealed, and a sealed session is never reopened");
    }
    let unfinished = unfinished_run(&mut tx, &session).await?;
    if let Some(holding) = unfinished.in_flight() {
        bail!("the run {holding} is still in flight in the session {id}");
    }
    if let Some(waiting) = unfinished.waiting() {
        work::stopping(&mut tx, &waiting, Exit::Succeeded).await?;
    }
    instance::archive_on_seal(&mut tx, &session).await?;

    let sealed_at = tx.sessions().seal(&session).await?;
    tx.commit().await?;

    let sealed = Session {
        state: SessionState::Sealed,
        sealed_at: Some(sealed_at),
        ..session
    };
    fanout::publish(Change::SessionSealed(&sealed));

    Ok(sealed)
}

/// Unattended sealing, through the same command a person seals with, so nothing here can
/// decide differently to `seal`.
pub async fn seal_idle(store: &Store) -> Result<Vec<Session>> {
    let mut sealed = Vec::new();

    for id in idle(store).await? {
        sealed.push(seal(store, id).await?);
    }

    Ok(sealed)
}

async fn idle(store: &Store) -> Result<Vec<SessionId>> {
    let mut tx = store.begin().await?;
    let mut idle = Vec::new();

    for session in tx.sessions().idle(Timestamp::now() - IDLE).await? {
        let holds_unpublished_work = match tx.sessions().kept_instance(session.id).await? {
            Some(kept) => {
                instance::unpublished(&session.checkout.repositories, kept.observed.as_deref())
                    .is_some()
            }
            None => false,
        };
        if !holds_unpublished_work && unfinished_run(&mut tx, &session).await?.idle() {
            idle.push(session.id);
        }
    }

    Ok(idle)
}

pub(crate) struct UnfinishedRun {
    run: Option<Run>,
    held_input: bool,
}

pub(crate) async fn unfinished_run(tx: &mut Tx<'_>, session: &Session) -> Result<UnfinishedRun> {
    Ok(match tx.sessions().unfinished_run(session).await? {
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

pub async fn show(store: &Store, id: SessionId) -> Result<Session> {
    store.begin().await?.sessions().get(id).await
}

pub async fn sessions(store: &Store, organization: &str) -> Result<Vec<Session>> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.sessions().all(&organization).await
}

pub async fn continuations(store: &Store, id: SessionId) -> Result<Vec<SessionId>> {
    store.begin().await?.sessions().continuations(id).await
}

pub async fn post(
    store: &Store,
    id: SessionId,
    participant: &str,
    message: &str,
) -> Result<Option<Run>> {
    let mut tx = store.begin().await?;
    let session = tx.sessions().get(id).await?;
    let run = post_in(&mut tx, &session, participant, message).await?;
    tx.commit().await?;

    Ok(run)
}

pub(crate) async fn post_in(
    tx: &mut Tx<'_>,
    session: &Session,
    participant: &str,
    message: &str,
) -> Result<Option<Run>> {
    session.accepts("message")?;

    let unfinished = unfinished_run(tx, session).await?;
    match unfinished.post_destination() {
        PostDestination::Start => {
            said(tx, session, participant, message).await?;
            Ok(Some(tx.sessions().enqueue_run(session, None).await?))
        }
        PostDestination::Brief => {
            said(tx, session, participant, message).await?;
            Ok(None)
        }
        PostDestination::Held => {
            tx.sessions()
                .add_pending_message(session, participant, message)
                .await?;
            Ok(None)
        }
        // Held even for a waiting Run: its next turn waits for an active-work slot.
        PostDestination::Wake(waiting) => {
            tx.sessions()
                .add_pending_message(session, participant, message)
                .await?;
            Ok(Some(waiting.clone()))
        }
    }
}

async fn said(tx: &mut Tx<'_>, session: &Session, participant: &str, message: &str) -> Result<()> {
    tx.log()
        .append(
            session,
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
    id: SessionId,
    from: Option<Cursor>,
    window: Window,
) -> Result<Page, Unreadable> {
    let mut tx = store.begin().await?;
    let session = tx.sessions().get(id).await?;

    tx.log().page(&session, from, window).await
}

pub async fn resolve(store: &Store, organization: &str, reference: &str) -> Result<Session> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;

    tx.sessions().resolved(&organization, reference).await
}

/// Only a sealed Session is continued: work an open one could still take belongs in it.
async fn continued(
    tx: &mut Tx<'_>,
    organization: &Organization,
    reference: &str,
) -> Result<Session> {
    let sealed = tx.sessions().resolved(organization, reference).await?;

    if sealed.state != SessionState::Sealed {
        bail!(
            "the session {} is open, and work continues in it rather than after it",
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
            session: SessionId::generate(),
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
