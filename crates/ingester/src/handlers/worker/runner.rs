//! Per-chain worker: acquire the chain, then alternate catch-up and live tail.

use crate::adapters::DynRpc;
use crate::app::config::{ChainConfig, redact_url};
use crate::app::state::WorkerDeps;
use crate::domain::error::IngesterError;
use crate::domain::models::parse_address;
use crate::handlers::worker::live::run as run_live;
use crate::repositories::ChainStateRepo;
use crate::services::backfill::BackfillService;
use crate::services::live::{LiveService, LiveServiceImpl};
use crate::services::retry::{Policy, retrying};
use alloy::primitives::Address;
use database::advisory::{ChainLock, NS_INGESTER, chain_key};
use shared::shutdown::Shutdown;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Floor on how often a standby retries, and how often the leader re-checks
/// its own lock. `block_poll_ms` can be very small; polling Postgres that hard
/// buys nothing here.
const LOCK_POLL_FLOOR: Duration = Duration::from_secs(1);

/// Why a worker stopped. The supervisor restarts on [`WorkerExit::LockLost`]
/// so the process re-queues for the chain instead of abandoning it, and stops
/// on [`WorkerExit::Shutdown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerExit {
    Shutdown,
    LockLost,
}

pub async fn run(deps: WorkerDeps, shutdown: Shutdown) -> Result<WorkerExit, IngesterError> {
    run_inner(deps, shutdown, true).await
}

/// Test entry point: optionally skip the advisory lock.
pub async fn run_inner(
    deps: WorkerDeps,
    mut shutdown: Shutdown,
    take_lock: bool,
) -> Result<WorkerExit, IngesterError> {
    let chain_id = deps.cfg.chain_id;
    let lock_poll = Duration::from_millis(deps.cfg.block_poll_ms).max(LOCK_POLL_FLOOR);
    info!(
        chain_id,
        // Never the raw URL: Alchemy, Infura and QuickNode all put the API key
        // in the path, and this line goes to every log consumer.
        rpc_url = %redact_url(&deps.cfg.rpc_url),
        pool_address = %deps.cfg.pool_address,
        start_block = deps.cfg.start_block,
        "worker starting"
    );

    let lock = if take_lock {
        match acquire(&deps.database_url, chain_id, lock_poll, shutdown.clone()).await? {
            Some(lock) => Some(lock),
            // Shutdown arrived while standing by; there is nothing to run.
            None => return Ok(WorkerExit::Shutdown),
        }
    } else {
        None
    };

    let chain = Chain::from_deps(deps)?;
    let body = chain.ingest();

    match lock {
        // Stop the moment the lock goes away rather than keep writing beside
        // whichever replica has taken over. Cancelling mid-batch is safe:
        // every write is transactional and `ON CONFLICT`-idempotent, so the
        // new leader redoes it.
        Some(lock) => tokio::select! {
            r = body => r.map(|()| WorkerExit::Shutdown),
            () = until_lock_lost(lock, lock_poll) => {
                error!(chain_id, "advisory lock lost; stopping worker to avoid two writers");
                Ok(WorkerExit::LockLost)
            }
            () = shutdown.recv() => {
                info!(chain_id, "shutdown signalled; releasing chain lock");
                Ok(WorkerExit::Shutdown)
            }
        },
        None => tokio::select! {
            r = body => r.map(|()| WorkerExit::Shutdown),
            () = shutdown.recv() => {
                info!(chain_id, "shutdown signalled");
                Ok(WorkerExit::Shutdown)
            }
        },
    }
}

/// Block until this process owns the chain, or shutdown is signalled.
///
/// Retrying rather than returning is what makes a standby replica useful: it
/// sits here until the leader exits or dies, then picks up the chain.
async fn acquire(
    database_url: &str,
    chain_id: i64,
    poll: Duration,
    mut shutdown: Shutdown,
) -> Result<Option<ChainLock>, IngesterError> {
    let key = chain_key(NS_INGESTER, chain_id);
    loop {
        match ChainLock::try_acquire(database_url, key).await {
            Ok(Some(lock)) => {
                info!(chain_id, "advisory lock acquired; ingesting");
                return Ok(Some(lock));
            }
            Ok(None) => {
                debug!(
                    chain_id,
                    "advisory lock held by another process; standing by"
                );
                tokio::select! {
                    () = tokio::time::sleep(poll) => {}
                    () = shutdown.recv() => return Ok(None),
                }
            }
            // Advisory errors are connection failures; surface them as db errors.
            Err(e) => return Err(IngesterError::Db(e.to_string())),
        }
    }
}

/// Resolves once the lock connection stops answering — the lock is then gone
/// server-side and a standby may already have taken the chain.
async fn until_lock_lost(mut lock: ChainLock, poll: Duration) {
    loop {
        tokio::time::sleep(poll).await;
        if !lock.is_alive().await {
            return;
        }
    }
}

/// One chain's ingest pipeline, with the lock plumbing left behind.
struct Chain {
    cfg: ChainConfig,
    pool_addr: Address,
    rpc: DynRpc,
    chain_state: Arc<dyn ChainStateRepo>,
    backfill: Arc<BackfillService>,
    live: Arc<dyn LiveService>,
}

impl Chain {
    fn from_deps(deps: WorkerDeps) -> Result<Self, IngesterError> {
        let WorkerDeps {
            cfg,
            rpc,
            chain_state,
            ingest,
            reorg,
            backfill,
            database_url: _,
        } = deps;
        let pool_addr = parse_address(&cfg.pool_address)?;
        let live = Arc::new(LiveServiceImpl {
            cfg: cfg.clone(),
            pool_addr,
            rpc: rpc.clone(),
            chain_state: chain_state.clone(),
            ingest,
            reorg,
        }) as Arc<dyn LiveService>;
        Ok(Self {
            cfg,
            pool_addr,
            rpc,
            chain_state,
            backfill,
            live,
        })
    }

    /// Alternate between catch-up and live tail for the life of the worker.
    ///
    /// The loop is the point: backfill used to run exactly once, at startup,
    /// so a worker that fell behind afterwards ground through the gap one poll
    /// interval at a time with no chunking and no parallelism.
    ///
    /// Never returns `Ok`; it ends only on error or cancellation.
    async fn ingest(&self) -> Result<(), IngesterError> {
        let chain_id = self.cfg.chain_id;
        loop {
            retrying(Policy::BACKFILL, "backfill", chain_id, || self.catch_up()).await?;
            info!(chain_id, "transitioning to live mode");
            let exit = run_live(self.live.clone()).await?;
            warn!(chain_id, ?exit, "live loop yielded; re-entering catch-up");
        }
    }

    /// Backfill up to a reorg-safe height, if far enough behind to be worth it.
    async fn catch_up(&self) -> Result<(), IngesterError> {
        let chain_id = self.cfg.chain_id;
        let last_scanned = self
            .chain_state
            .fetch(chain_id)
            .await?
            .map(|c| c.last_scanned_block)
            .unwrap_or(self.cfg.start_block - 1);
        let tip = self.rpc.tip().await? as i64;
        // In i64 throughout: `last_scanned` is -1 before the first commit, and
        // casting that to u64 made the lag u64::MAX and silently skipped
        // backfill entirely.
        let lag = tip - last_scanned;
        info!(chain_id, last_scanned, tip, lag, "chain state resolved");

        if lag <= self.cfg.backfill_threshold as i64 {
            info!(
                chain_id,
                lag,
                threshold = self.cfg.backfill_threshold,
                "skipping backfill"
            );
            return Ok(());
        }

        let safe_to = tip - self.cfg.reorg_depth as i64;
        if safe_to <= last_scanned {
            return Ok(());
        }
        info!(
            chain_id,
            from = last_scanned + 1,
            to = safe_to,
            threshold = self.cfg.backfill_threshold,
            "entering backfill"
        );
        self.backfill
            .run(
                &self.cfg,
                self.pool_addr,
                (last_scanned + 1) as u64,
                safe_to as u64,
            )
            .await?;
        info!(chain_id, "backfill complete");
        Ok(())
    }
}
