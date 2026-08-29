//! What each role does once argv has selected it.

pub mod serve;
pub mod work;

use tokio_util::sync::CancellationToken;

/// Runs every long-running role in one process, returning when all of them have stopped.
///
/// This is the default, and at 0.1 the only supported topology: splitting the roles needs
/// an out-of-process `Fanout` and `Timer`, which would drag Redis or Postgres into rung one
/// (ADR-0002).
///
/// One process has one lifetime, so a role returning — cleanly or not — asks the others to
/// stop rather than leaving a half-dead process behind.
pub async fn all_in_one(shutdown: CancellationToken) -> anyhow::Result<()> {
    let serve = tokio::spawn(stopping_the_others(shutdown.clone(), serve::run));
    let work = tokio::spawn(stopping_the_others(shutdown.clone(), work::run));

    let (serve, work) = tokio::join!(serve, work);
    serve??;
    work??;
    Ok(())
}

/// Runs one role, cancelling `shutdown` on the way out so its siblings stop too.
async fn stopping_the_others<Role, Running>(
    shutdown: CancellationToken,
    role: Role,
) -> anyhow::Result<()>
where
    Role: FnOnce(CancellationToken) -> Running,
    Running: Future<Output = anyhow::Result<()>>,
{
    let _stop_the_others = shutdown.clone().drop_guard();
    role(shutdown).await
}
