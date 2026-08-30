pub mod cli;
pub mod serve;
pub mod work;

use std::net::SocketAddr;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::store::Store;

pub struct AllInOne {
    listening: serve::Listening,
}

pub async fn bind(store: Store, listen: SocketAddr) -> Result<AllInOne> {
    Ok(AllInOne {
        listening: serve::bind(store, listen).await?,
    })
}

impl AllInOne {
    pub fn address(&self) -> SocketAddr {
        self.listening.address()
    }

    pub async fn run(self, shutdown: CancellationToken) -> Result<()> {
        let serve = tokio::spawn(stopping_the_others(shutdown.clone(), |shutdown| {
            serve::run(self.listening, shutdown)
        }));
        let work = tokio::spawn(stopping_the_others(shutdown.clone(), work::run));

        let (serve, work) = tokio::join!(serve, work);
        serve??;
        work??;
        Ok(())
    }
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
