use crate::app::state::WorkerDeps;
use crate::domain::error::IngesterError;
use crate::domain::models::parse_address;
use crate::handlers::worker::live::run as run_live;
use crate::services::live::LiveServiceImpl;
use database::advisory::{ChainLock, NS_INGESTER, chain_key};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info};

/// Floor on how often a standby retries, and how often the leader re-checks
/// its own lock. `block_poll_ms` can be very small; polling Postgres that hard
/// buys nothing here.
const LOCK_POLL_FLOOR: Duration = Duration::from_secs(1);

pub async fn run(deps: WorkerDeps) -> Result<(), IngesterError> {
    run_inner(deps, true).await
}

/// Test entry point: optionally skip the advisory lock.
pub async fn run_inner(deps: WorkerDeps, take_lock: bool) -> Result<(), IngesterError> {
    let WorkerDeps {
        cfg,
        rpc,
        chain_state,
        ingest,
        reorg,
        backfill,
        database_url,
    } = deps;
    let chain_id = cfg.chain_id;
    let lock_poll = Duration::from_millis(cfg.block_poll_ms).max(LOCK_POLL_FLOOR);
    info!(
        chain_id,
        rpc_url = %cfg.rpc_url,
        pool_address = %cfg.pool_address,
        start_block = cfg.start_block,
        "worker starting"
    );

    let lock = if take_lock {
        Some(acquire(&database_url, chain_id, lock_poll).await?)
    } else {
        None
    };

    let pool_addr = parse_address(&cfg.pool_address)?;
    let body = ingest_chain(cfg, pool_addr, rpc, chain_state, ingest, reorg, backfill);

    match lock {
        // Stop the moment the lock goes away rather than keep writing beside
        // whichever replica has taken over. Cancelling mid-batch is safe: every
        // write is `ON CONFLICT`-idempotent, so the new leader redoes it.
        Some(lock) => tokio::select! {
            r = body => r,
            () = until_lock_lost(lock, lock_poll) => {
                error!(chain_id, "advisory lock lost; stopping worker to avoid two writers");
                Ok(())
            }
        },
        None => body.await,
    }
}

/// Block until this process owns the chain.
///
/// Retrying rather than returning is what makes a standby replica useful: it
/// sits here until the leader exits or dies, then picks up the chain.
async fn acquire(
    database_url: &str,
    chain_id: i64,
    poll: Duration,
) -> Result<ChainLock, IngesterError> {
    let key = chain_key(NS_INGESTER, chain_id);
    loop {
        match ChainLock::try_acquire(database_url, key).await {
            Ok(Some(lock)) => {
                info!(chain_id, "advisory lock acquired; ingesting");
                return Ok(lock);
            }
            Ok(None) => {
                debug!(
                    chain_id,
                    "advisory lock held by another process; standing by"
                );
                tokio::time::sleep(poll).await;
            }
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

async fn ingest_chain(
    cfg: crate::app::config::ChainConfig,
    pool_addr: alloy::primitives::Address,
    rpc: crate::adapters::DynRpc,
    chain_state: Arc<dyn crate::repositories::ChainStateRepo>,
    ingest: Arc<crate::services::ingest::IngestService>,
    reorg: Arc<crate::services::reorg::ReorgService>,
    backfill: Arc<crate::services::backfill::BackfillService>,
) -> Result<(), IngesterError> {
    let chain_id = cfg.chain_id;
    let last_scanned = chain_state
        .fetch(chain_id)
        .await?
        .map(|s| s.last_scanned_block)
        .unwrap_or(cfg.start_block - 1);
    let tip_n = rpc.tip().await?;
    let lag = tip_n.saturating_sub(last_scanned as u64);
    info!(
        chain_id,
        last_scanned,
        tip = tip_n,
        lag,
        "chain state resolved"
    );

    if lag > cfg.backfill_threshold {
        let safe_to = tip_n.saturating_sub(cfg.reorg_depth);
        if safe_to > last_scanned as u64 {
            info!(
                chain_id,
                from = (last_scanned as u64) + 1,
                to = safe_to,
                threshold = cfg.backfill_threshold,
                "entering backfill"
            );
            if let Err(e) = backfill
                .run(&cfg, pool_addr, (last_scanned as u64) + 1, safe_to)
                .await
            {
                error!(chain_id, "backfill error: {}", e);
                return Err(e);
            }
            info!(chain_id, "backfill complete");
        }
    } else {
        info!(
            chain_id,
            lag,
            threshold = cfg.backfill_threshold,
            "skipping backfill"
        );
    }

    info!(chain_id, "transitioning to live mode");
    let svc = Arc::new(LiveServiceImpl {
        cfg,
        pool_addr,
        rpc,
        chain_state,
        ingest,
        reorg,
    });
    run_live(svc).await
}
