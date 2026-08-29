pub mod serve;
pub mod work;

use tokio_util::sync::CancellationToken;

pub async fn all_in_one(shutdown: CancellationToken) -> anyhow::Result<()> {
    let serve = tokio::spawn(stopping_the_others(shutdown.clone(), serve::run));
    let work = tokio::spawn(stopping_the_others(shutdown.clone(), work::run));

    let (serve, work) = tokio::join!(serve, work);
    serve??;
    work??;
    Ok(())
}

/// The drop guard fires however `role` returns, so no sibling outlives it.
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
