use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::domain::{Session, SessionId};
use crate::log::Entry;
use crate::session;
use crate::store::session::Kept;
use crate::store::{Store, Tx};

pub enum Admission {
    Available,
    Waiting(String),
}

pub async fn admit(tx: &mut Tx<'_>, session: &Session) -> Result<Admission> {
    if tx.sessions().instance(session.id).await?.is_some() {
        return Ok(Admission::Available);
    }
    let Some(limit) = session.organization.max_live_instances else {
        return Ok(Admission::Available);
    };
    if tx
        .sessions()
        .live_instance_count(&session.organization)
        .await?
        < limit.get()
    {
        return Ok(Admission::Available);
    }
    if let Some(instance) = tx
        .sessions()
        .instance_being_archived(&session.organization)
        .await?
    {
        return Ok(Admission::Waiting(format!(
            "waiting for the idle Instance {instance} to be archived"
        )));
    }

    for kept in tx.sessions().kept_instances(&session.organization).await? {
        let candidate = tx.sessions().get(kept.session).await?;
        if session::unfinished_run(tx, &candidate).await?.idle()
            && unpublished(&candidate.checkout.repositories, kept.observed.as_deref()).is_none()
        {
            tx.sessions()
                .archive_instance(&candidate, &kept.instance)
                .await?;
            return Ok(Admission::Waiting(format!(
                "waiting for the idle Instance {} to be archived",
                kept.instance
            )));
        }
    }

    Ok(Admission::Waiting(format!(
        "the organization {} has reached its limit of {} live Instance{}; none idle is known recoverable",
        session.organization.name,
        limit,
        if limit.get() == 1 { "" } else { "s" }
    )))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed {
    pub repository: String,
    #[serde(flatten)]
    pub git: Git,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "git", rename_all = "snake_case")]
pub enum Git {
    Read {
        branch: Option<String>,
        untracked: u64,
        uncommitted: u64,
        stashes: u64,
        unpushed: u64,
    },
    Unreadable {
        because: String,
    },
}

/// A checkout nobody reported on is judged to hold work, since nothing says it does not.
pub fn unpublished(repositories: &[String], observed: Option<&[Observed]>) -> Option<String> {
    let Some(observed) = observed else {
        return Some("no run reported what its checkout holds".to_owned());
    };

    let mut held: Vec<String> = repositories
        .iter()
        .filter(|repository| {
            !observed
                .iter()
                .any(|item| item.repository == repository.as_str())
        })
        .map(|repository| format!("{repository} was not reported"))
        .collect();
    held.extend(observed.iter().filter_map(held_in));
    (!held.is_empty()).then(|| held.join("; "))
}

fn held_in(observed: &Observed) -> Option<String> {
    let repository = &observed.repository;
    let (branch, counted) = match &observed.git {
        Git::Unreadable { because } => {
            return Some(format!("{repository} could not be read: {because}"));
        }
        Git::Read {
            branch,
            untracked,
            uncommitted,
            stashes,
            unpushed,
        } => (
            branch,
            [
                (*unpushed, "unpushed commit"),
                (*uncommitted, "uncommitted change"),
                (*untracked, "untracked file"),
                (*stashes, "stash"),
            ],
        ),
    };

    let held: Vec<String> = counted
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| match count {
            1 => format!("1 {what}"),
            _ if what.ends_with('h') => format!("{count} {what}es"),
            _ => format!("{count} {what}s"),
        })
        .collect();
    if held.is_empty() {
        return None;
    }

    Some(match branch {
        Some(branch) => format!("{repository} on {branch} has {}", held.join(", ")),
        None => format!("{repository} has {}", held.join(", ")),
    })
}

#[derive(Debug, Clone)]
pub struct Held {
    pub session: SessionId,
    pub instance: String,
    pub because: String,
}

pub async fn held(store: &Store, organization: &str) -> Result<Vec<Held>> {
    let mut tx = store.begin().await?;
    let organization = tx.organizations().named(organization).await?;
    let mut held = Vec::new();

    for kept in tx.sessions().kept_instances(&organization).await? {
        if let Some(judged) = judged(&mut tx, kept).await? {
            held.push(judged);
        }
    }

    Ok(held)
}

pub async fn held_by(store: &Store, session: SessionId) -> Result<Option<Held>> {
    let mut tx = store.begin().await?;
    let Some(kept) = tx.sessions().kept_instance(session).await? else {
        return Ok(None);
    };

    judged(&mut tx, kept).await
}

/// An Instance a run is using or about to use is not held: what it holds is not yet known.
async fn judged(tx: &mut Tx<'_>, kept: Kept) -> Result<Option<Held>> {
    let session = tx.sessions().get(kept.session).await?;
    if session::unfinished_run(tx, &session)
        .await?
        .in_flight()
        .is_some()
    {
        return Ok(None);
    }

    Ok(
        unpublished(&session.checkout.repositories, kept.observed.as_deref()).map(|because| Held {
            session: kept.session,
            instance: kept.instance,
            because,
        }),
    )
}

/// The one way work that may exist nowhere else is ever discarded, so only a person calls it.
pub async fn release(store: &Store, id: SessionId, participant: &str) -> Result<String> {
    let mut tx = store.begin().await?;
    let session = tx.sessions().get(id).await?;
    session.accepts("release")?;

    let Some(kept) = tx.sessions().kept_instance(id).await? else {
        bail!("the session {id} has no instance to release");
    };
    if let Some(holding) = session::unfinished_run(&mut tx, &session)
        .await?
        .in_flight()
    {
        bail!(
            "the run {holding} is still in flight on the instance {}",
            kept.instance
        );
    }

    tx.log()
        .append(
            &session,
            Entry::InstanceReleased {
                participant: participant.to_owned(),
                instance: kept.instance.clone(),
                unpublished: unpublished(&session.checkout.repositories, kept.observed.as_deref()),
            },
        )
        .await?;
    tx.sessions()
        .archive_instance(&session, &kept.instance)
        .await?;
    tx.commit().await?;

    Ok(kept.instance)
}

pub(crate) async fn archive_on_seal(tx: &mut Tx<'_>, session: &Session) -> Result<()> {
    let Some(kept) = tx.sessions().kept_instance(session.id).await? else {
        return Ok(());
    };

    if let Some(because) = unpublished(&session.checkout.repositories, kept.observed.as_deref()) {
        bail!(
            "the session {}'s instance {} may hold the only copy of its work ({because}); publish \
             it from a follow-up run, or release the instance to discard it",
            session.id,
            kept.instance
        );
    }

    tx.sessions()
        .archive_instance(session, &kept.instance)
        .await
}

pub async fn to_archive(store: &Store) -> Result<Vec<String>> {
    store.begin().await?.sessions().instances_to_archive().await
}

pub async fn archived(store: &Store, instance: &str) -> Result<()> {
    let mut tx = store.begin().await?;
    tx.sessions().instance_archived(instance).await?;

    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(untracked: u64, uncommitted: u64, stashes: u64, unpushed: u64) -> Observed {
        Observed {
            repository: "https://github.com/acme/widgets".to_owned(),
            git: Git::Read {
                branch: Some("kestrel/work".to_owned()),
                untracked,
                uncommitted,
                stashes,
                unpushed,
            },
        }
    }

    #[test]
    fn a_checkout_nobody_reported_on_is_held() {
        assert!(unpublished(&[], None).is_some());
    }

    #[test]
    fn a_clean_pushed_checkout_holds_nothing() {
        assert_eq!(
            unpublished(
                &["https://github.com/acme/widgets".to_owned()],
                Some(&[read(0, 0, 0, 0)])
            ),
            None
        );
    }

    #[test]
    fn a_workspace_with_no_repositories_holds_nothing() {
        assert_eq!(unpublished(&[], Some(&[])), None);
    }

    #[test]
    fn a_repository_missing_from_a_report_is_held() {
        assert_eq!(
            unpublished(&["https://github.com/acme/widgets".to_owned()], Some(&[])).as_deref(),
            Some("https://github.com/acme/widgets was not reported")
        );
    }

    #[test]
    fn each_kind_of_local_work_is_held_on_its_own() {
        for observed in [
            read(1, 0, 0, 0),
            read(0, 1, 0, 0),
            read(0, 0, 1, 0),
            read(0, 0, 0, 1),
        ] {
            assert!(
                unpublished(
                    &["https://github.com/acme/widgets".to_owned()],
                    Some(std::slice::from_ref(&observed))
                )
                .is_some(),
                "{observed:?} was judged to hold nothing"
            );
        }
    }

    #[test]
    fn the_reason_names_the_repository_its_branch_and_what_it_holds() {
        assert_eq!(
            unpublished(
                &["https://github.com/acme/widgets".to_owned()],
                Some(&[read(1, 0, 2, 3)])
            )
            .as_deref(),
            Some(
                "https://github.com/acme/widgets on kestrel/work has 3 unpushed commits, \
                 1 untracked file, 2 stashes"
            )
        );
    }

    #[test]
    fn an_unreadable_repository_is_held_even_beside_a_clean_one() {
        let unreadable = Observed {
            repository: "https://github.com/acme/gadgets".to_owned(),
            git: Git::Unreadable {
                because: "there is no checkout at gadgets".to_owned(),
            },
        };

        assert_eq!(
            unpublished(
                &[
                    "https://github.com/acme/widgets".to_owned(),
                    "https://github.com/acme/gadgets".to_owned(),
                ],
                Some(&[read(0, 0, 0, 0), unreadable])
            )
            .as_deref(),
            Some(
                "https://github.com/acme/gadgets could not be read: there is no checkout at gadgets"
            )
        );
    }

    #[test]
    fn a_report_reads_as_the_supervisor_sends_it() {
        let sent = serde_json::json!({
            "repository": "https://github.com/acme/widgets",
            "git": "read",
            "branch": null,
            "untracked": 0,
            "uncommitted": 1,
            "stashes": 0,
            "unpushed": 0,
        });

        let observed: Observed = serde_json::from_value(sent).expect("an observation");

        assert_eq!(
            observed.git,
            Git::Read {
                branch: None,
                untracked: 0,
                uncommitted: 1,
                stashes: 0,
                unpushed: 0,
            }
        );
    }
}
