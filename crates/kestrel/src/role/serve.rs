use std::net::SocketAddr;

use anyhow::{Context as _, Result};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::cli::Role;
use crate::link;
use crate::store::Store;

pub struct Listening {
    listener: TcpListener,
    address: SocketAddr,
    store: Store,
}

impl Listening {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

/// Binding before the role starts is what lets a caller that asked for port 0 learn which
/// port it got.
pub async fn bind(store: Store, listen: SocketAddr) -> Result<Listening> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("listening on {listen}"))?;
    let address = listener.local_addr()?;

    Ok(Listening {
        listener,
        address,
        store,
    })
}

pub async fn run(listening: Listening, shutdown: CancellationToken) -> Result<()> {
    let Listening {
        listener,
        address,
        store,
    } = listening;

    info!(role = %Role::Serve, %address, "role started");
    axum::serve(listener, link::router(store, shutdown.clone()))
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await
        .context("serving the link")?;
    info!(role = %Role::Serve, "role stopped");

    Ok(())
}
