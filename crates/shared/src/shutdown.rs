//! Graceful-shutdown primitive shared by every long-running binary.
//!
//! Single producer (the signal handler) to many consumers (worker tasks).
//! Workers hold a `Shutdown` and await `.recv()` to be woken when stop is
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

    /// Whether stop has already been signalled, without awaiting.
    ///
    /// A loop that skips its `recv().await`, as the tick driver does while a
    /// service still has queued work, would otherwise never observe shutdown and
    /// could not be stopped during a long catch-up.
    pub fn is_triggered(&self) -> bool {
        *self.rx.borrow()
    }
}

impl ShutdownTrigger {
    pub fn fire(&self) {
        let _ = self.tx.send(true);
    }
}

/// Resolve on ctrl-c, or on SIGTERM where the platform has one.
///
/// Hand this to `axum::serve(..).with_graceful_shutdown` in a binary that serves
/// requests and has no worker tasks to signal.
pub async fn signal() {
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
}

/// Wait for [`signal`] and fire the trigger once.
pub async fn watch_signals(trigger: ShutdownTrigger) {
    signal().await;
    trigger.fire();
}
