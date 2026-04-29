//! Graceful-shutdown primitive shared by every long-running binary.
//!
//! Single producer (signal handler) → many consumers (worker tasks).
//! Workers hold a `Shutdown` and `.recv().await` to be woken when stop is
//! signalled.

use tokio::sync::watch;
use tracing::info;

#[derive(Clone)]
pub struct Shutdown {
    rx: watch::Receiver<bool>,
}

pub struct ShutdownTrigger {
    tx: watch::Sender<bool>,
}

pub fn channel() -> (ShutdownTrigger, Shutdown) {
    let (tx, rx) = watch::channel(false);
    (ShutdownTrigger { tx }, Shutdown { rx })
}

impl Shutdown {
    pub async fn recv(&mut self) {
        let _ = self.rx.changed().await;
    }
}

impl ShutdownTrigger {
    pub fn fire(&self) {
        let _ = self.tx.send(true);
    }
}

/// Wait for ctrl-c (or SIGTERM on unix) and fire the trigger once.
pub async fn watch_signals(trigger: ShutdownTrigger) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => info!("ctrl-c received"),
            _ = term.recv() => info!("SIGTERM received"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("ctrl-c received");
    }
    trigger.fire();
}
