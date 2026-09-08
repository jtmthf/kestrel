pub mod agent;
pub mod integration;
pub mod organization;
pub mod session;
pub mod trigger;
pub mod workspace;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use jiff::Timestamp;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteRow};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::keyring::Keyring;
use crate::log::Log;
use crate::store::agent::Agents;
use crate::store::integration::Integrations;
use crate::store::organization::Organizations;
use crate::store::session::Sessions;
use crate::store::trigger::Triggers;
use crate::store::workspace::Workspaces;

const DATABASE: &str = "kestrel.db";

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    keyring: Arc<Keyring>,
}

impl Store {
    pub async fn open(data_dir: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(data_dir)
            .await
            .with_context(|| format!("creating kestrel's data directory {}", data_dir.display()))?;

        let options = SqliteConnectOptions::new()
            .filename(data_dir.join(DATABASE))
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);

        let pool = SqlitePool::connect_with(options)
            .await
            .with_context(|| format!("opening kestrel's database in {}", data_dir.display()))?;

        sqlx::migrate!("src/store/migrations")
            .run(&pool)
            .await
            .context("migrating kestrel's database")?;

        Ok(Self {
            pool,
            keyring: Arc::new(Keyring::beside(data_dir)?),
        })
    }

    /// A `Tx` that is dropped rather than committed rolls back, which is how a read is scoped
    /// too. Every one of them takes the write lock up front: SQLite refuses a deferred
    /// transaction that reads and then writes while another has written, rather than making
    /// it wait its turn.
    pub async fn begin(&self) -> Result<Tx<'_>> {
        Ok(Tx {
            transaction: self.pool.begin_with("BEGIN IMMEDIATE").await?,
            keyring: &self.keyring,
        })
    }
}

pub struct Tx<'a> {
    transaction: Transaction<'a, Sqlite>,
    keyring: &'a Keyring,
}

impl Tx<'_> {
    pub fn log(&mut self) -> Log<'_> {
        Log::over(&mut self.transaction)
    }

    pub fn organizations(&mut self) -> Organizations<'_> {
        Organizations::over(&mut self.transaction, self.keyring)
    }

    pub fn workspaces(&mut self) -> Workspaces<'_> {
        Workspaces::over(&mut self.transaction)
    }

    pub fn agents(&mut self) -> Agents<'_> {
        Agents::over(&mut self.transaction)
    }

    pub fn sessions(&mut self) -> Sessions<'_> {
        Sessions::over(&mut self.transaction)
    }

    pub fn integrations(&mut self) -> Integrations<'_> {
        Integrations::over(&mut self.transaction)
    }

    pub fn triggers(&mut self) -> Triggers<'_> {
        Triggers::over(&mut self.transaction)
    }

    pub async fn commit(self) -> Result<()> {
        self.transaction.commit().await?;
        Ok(())
    }
}

/// A due time is the one timestamp SQL compares rather than reads back, and at the precision
/// jiff prints by default a whole second sorts after the fractions of it.
fn due(at: Timestamp) -> String {
    format!("{at:.9}")
}

fn timestamp(row: &SqliteRow, column: &str) -> Result<Option<Timestamp>> {
    row.get::<Option<String>, _>(column)
        .map(|at| at.parse())
        .transpose()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::domain::{Agent, Organization, Run, Workspace};
    use crate::log::Entry;
    use crate::store::session::Taken;

    async fn declared(store: &Store) -> (Organization, Workspace, Agent) {
        let mut tx = store.begin().await.unwrap();
        let organization = tx.organizations().declare("acme").await.unwrap();
        let workspace = tx
            .workspaces()
            .declare(
                &organization,
                "kestrel",
                &["https://github.com/jtmthf/kestrel".to_owned()],
                "main",
            )
            .await
            .unwrap();
        let agent = tx
            .agents()
            .declare(&organization, "builder", "opencode", Some("claude-opus-5"))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        (organization, workspace, agent)
    }

    #[test]
    fn a_due_time_at_a_whole_second_sorts_before_the_moments_after_it() {
        let whole: Timestamp = "2026-09-01T12:00:00Z".parse().unwrap();
        let after: Timestamp = "2026-09-01T12:00:00.5Z".parse().unwrap();

        assert!(due(whole) < due(after));
        assert_eq!(due(whole).parse::<Timestamp>().unwrap(), whole);
    }

    async fn a_run(store: &Store) -> Run {
        let (organization, workspace, agent) = declared(store).await;

        let mut tx = store.begin().await.unwrap();
        let session = tx
            .sessions()
            .open(&organization, &workspace, &agent, None, None)
            .await
            .unwrap();
        let run = tx.sessions().enqueue_run(&session).await.unwrap();
        tx.commit().await.unwrap();

        run
    }

    async fn entries(store: &Store) -> i64 {
        let mut tx = store.begin().await.unwrap();
        sqlx::query("SELECT COUNT(*) AS entries FROM transcript_entry")
            .fetch_one(&mut *tx.transaction)
            .await
            .unwrap()
            .get("entries")
    }

    #[tokio::test]
    async fn a_report_is_taken_once_and_a_replay_of_it_changes_nothing() {
        let data_dir = TempDir::new().unwrap();
        let store = Store::open(data_dir.path()).await.unwrap();
        let run = a_run(&store).await;

        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.sessions().take_report(&run, 1).await.unwrap(),
            Taken::Next
        ));
        assert!(matches!(
            tx.sessions().take_report(&run, 1).await.unwrap(),
            Taken::Again
        ));
        assert!(matches!(
            tx.sessions().take_report(&run, 3).await.unwrap(),
            Taken::Skipped
        ));
        assert!(matches!(
            tx.sessions().take_report(&run, 2).await.unwrap(),
            Taken::Next
        ));
    }

    #[tokio::test]
    async fn a_report_taken_in_a_transaction_that_rolls_back_is_the_next_one_again() {
        let data_dir = TempDir::new().unwrap();
        let store = Store::open(data_dir.path()).await.unwrap();
        let run = a_run(&store).await;
        let session = store
            .begin()
            .await
            .unwrap()
            .sessions()
            .get(run.session)
            .await
            .unwrap();

        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.sessions().take_report(&run, 1).await.unwrap(),
            Taken::Next
        ));
        tx.log()
            .append(
                &session,
                Entry::Said {
                    participant: "builder".to_owned(),
                    message: "lost with the answer to it".to_owned(),
                },
            )
            .await
            .unwrap();
        drop(tx);

        assert_eq!(entries(&store).await, 0);
        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.sessions().take_report(&run, 1).await.unwrap(),
            Taken::Next
        ));
    }

    #[tokio::test]
    async fn a_transaction_that_fails_part_way_leaves_neither_the_session_nor_its_entry() {
        let data_dir = TempDir::new().unwrap();
        let store = Store::open(data_dir.path()).await.unwrap();
        let (organization, workspace, agent) = declared(&store).await;

        let mut tx = store.begin().await.unwrap();
        let session = tx
            .sessions()
            .open(&organization, &workspace, &agent, None, None)
            .await
            .unwrap();
        tx.log()
            .append(
                &session,
                Entry::ParticipantJoined {
                    participant: agent.name.clone(),
                },
            )
            .await
            .unwrap();
        drop(tx);

        let mut tx = store.begin().await.unwrap();
        assert!(tx.sessions().get(session.id).await.is_err());
        let entries: i64 = sqlx::query("SELECT COUNT(*) AS entries FROM transcript_entry")
            .fetch_one(&mut *tx.transaction)
            .await
            .unwrap()
            .get("entries");
        assert_eq!(entries, 0);
    }
}
