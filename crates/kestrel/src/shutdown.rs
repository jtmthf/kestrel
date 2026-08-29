use tokio_util::sync::CancellationToken;
use tracing::info;

pub fn on_signal() -> std::io::Result<CancellationToken> {
    let token = CancellationToken::new();
    let signalled = token.clone();

    let mut signals = Signals::listen()?;
    tokio::spawn(async move {
        let signal = signals.first().await;
        info!(signal, "signalled; asking every role to stop");
        signalled.cancel();
    });

    Ok(token)
}

#[cfg(unix)]
struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn listen() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    async fn first(&mut self) -> &'static str {
        tokio::select! {
            _ = self.interrupt.recv() => "SIGINT",
            _ = self.terminate.recv() => "SIGTERM",
        }
    }
}

#[cfg(not(unix))]
struct Signals;

#[cfg(not(unix))]
impl Signals {
    fn listen() -> std::io::Result<Self> {
        Ok(Self)
    }

    async fn first(&mut self) -> &'static str {
        let _ = tokio::signal::ctrl_c().await;
        "ctrl-c"
    }
}
