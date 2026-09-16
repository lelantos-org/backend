//! Keeping every chain's worker alive, and reporting when one could not be.

use crate::app::state::WorkerDeps;
use crate::domain::error::IngesterError;
use crate::handlers::worker::runner::{self, WorkerExit};
use crate::services::retry::{Policy, is_retryable};
use shared::shutdown::Shutdown;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Spawn one supervised worker per chain.
pub fn spawn(
    deps: Vec<WorkerDeps>,
    shutdown: &Shutdown,
) -> Vec<(i64, JoinHandle<Result<(), IngesterError>>)> {
    deps.into_iter()
        .map(|deps| {
            let chain_id = deps.cfg.chain_id;
            let shutdown = shutdown.clone();
            (chain_id, tokio::spawn(supervise(deps, shutdown)))
        })
        .collect()
}

/// Wait for every chain worker, then report which of them failed.
///
/// A dead chain is a failed process: exiting 0 after every worker gave up would
/// present a stalled ingester as healthy to its orchestrator.
pub async fn await_all(
    workers: Vec<(i64, JoinHandle<Result<(), IngesterError>>)>,
) -> Result<(), Vec<i64>> {
    let mut failed = Vec::new();
    for (chain_id, handle) in workers {
        match handle.await {
            Ok(Ok(())) => info!(chain_id, "worker stopped cleanly"),
            Ok(Err(e)) => {
                error!(chain_id, "worker exited: {}", e);
                failed.push(chain_id);
            }
            Err(e) => {
                error!(chain_id, "worker task panicked: {}", e);
                failed.push(chain_id);
            }
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed)
    }
}

/// Keep one chain's worker alive across recoverable failures.
///
/// A worker that loses its advisory lock is restarted rather than abandoned: it
/// returns to standby and retakes the chain when the lock frees. That is not a
/// failure and does not consume a restart.
async fn supervise(deps: WorkerDeps, mut shutdown: Shutdown) -> Result<(), IngesterError> {
    let chain_id = deps.cfg.chain_id;
    let policy = Policy::WORKER_RESTART;
    let mut restarts: u32 = 0;
    loop {
        match runner::run(deps.clone(), shutdown.clone()).await {
            Ok(WorkerExit::Shutdown) => return Ok(()),
            Ok(WorkerExit::LockLost) => {
                warn!(chain_id, "lock lost; returning to standby");
            }
            Err(e) if !is_retryable(&e) => return Err(e),
            Err(e) => {
                restarts += 1;
                if restarts >= policy.max_attempts {
                    error!(chain_id, restarts, "worker exhausted restarts: {}", e);
                    return Err(e);
                }
                let delay = policy.delay(restarts - 1);
                warn!(
                    chain_id,
                    restarts,
                    delay_ms = delay.as_millis() as u64,
                    "worker failed; restarting: {}",
                    e
                );
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = shutdown.recv() => return Ok(()),
                }
            }
        }
    }
}
