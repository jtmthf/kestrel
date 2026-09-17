use std::net::SocketAddr;

use anyhow::{Context as _, Result};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::cli::Role;
use crate::integration::webhook;
use crate::store::Store;
use crate::timer::Wake;
use crate::{link, operator};

/// Two listeners rather than one, so exposing the operator boundary never exposes the link
/// (ADR-0015).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listen {
    pub link: SocketAddr,
    pub operator: SocketAddr,
}

pub struct Listening {
    link: TcpListener,
    operator: TcpListener,
    bound: Listen,
    store: Store,
    wake: Wake,
}

impl Listening {
    pub fn bound(&self) -> Listen {
        self.bound
    }
}

/// Binding before the role starts is what lets a caller that asked for port 0 learn which
/// port it got.
pub async fn bind(store: Store, listen: Listen, wake: Wake) -> Result<Listening> {
    let link = TcpListener::bind(listen.link)
        .await
        .with_context(|| format!("listening for the link on {}", listen.link))?;
    let operator = TcpListener::bind(listen.operator)
        .await
        .with_context(|| format!("listening for operators on {}", listen.operator))?;
    let bound = Listen {
        link: link.local_addr()?,
        operator: operator.local_addr()?,
    };

    Ok(Listening {
        link,
        operator,
        bound,
        store,
        wake,
    })
}

pub async fn run(listening: Listening, shutdown: CancellationToken) -> Result<()> {
    let Listening {
        link: link_listener,
        operator: operator_listener,
        bound,
        store,
        wake,
    } = listening;

    info!(role = %Role::Serve, link = %bound.link, operator = %bound.operator, "role started");
    if !bound.operator.ip().is_loopback() {
        warn!(
            operator = %bound.operator,
            "the operator boundary authenticates nobody, and is listening beyond loopback"
        );
    }

    let link_router =
        link::router(store.clone(), shutdown.clone()).merge(webhook::router(store.clone(), wake));
    let operator_router = operator::router(store, shutdown.clone());

    let serving_link = axum::serve(link_listener, link_router)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned());
    let serving_operators = axum::serve(operator_listener, operator_router)
        .with_graceful_shutdown(shutdown.cancelled_owned());
    let (link_served, operators_served) =
        tokio::join!(serving_link.into_future(), serving_operators.into_future());
    link_served.context("serving the link and the webhooks")?;
    operators_served.context("serving operators")?;
    info!(role = %Role::Serve, "role stopped");

    Ok(())
}
