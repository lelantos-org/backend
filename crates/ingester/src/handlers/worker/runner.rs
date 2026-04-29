use crate::app::state::WorkerDeps;
use crate::domain::error::IngesterError;
use crate::domain::models::parse_address;
use crate::handlers::worker::live::run as run_live;
use crate::services::live::LiveServiceImpl;
use std::sync::Arc;
use tracing::{error, info};

const ADVISORY_NAMESPACE: i64 = 0x1A95_0000_0000_0000_u64 as i64;

pub fn advisory_key(chain_id: i64) -> i64 {
    ADVISORY_NAMESPACE ^ chain_id
}

pub async fn run(deps: WorkerDeps) -> Result<(), IngesterError> {
    run_inner(deps, true).await
}

/// Test entry point: optionally skip advisory lock.
pub async fn run_inner(deps: WorkerDeps, take_lock: bool) -> Result<(), IngesterError> {
    let WorkerDeps {
        cfg,
        rpc,
        raw_events,
        chain_state,
        ingest,
        reorg,
        backfill,
    } = deps;
    let chain_id = cfg.chain_id;
    info!(
        chain_id,
        rpc_url = %cfg.rpc_url,
        pool_address = %cfg.pool_address,
        start_block = cfg.start_block,
        "worker starting"
    );

    if take_lock {
        let key = advisory_key(chain_id);
        if !raw_events.try_advisory_lock(key).await? {
            info!(chain_id, "advisory lock held by another process; skipping");
            return Ok(());
        }
        info!(chain_id, "advisory lock acquired");
    }

    let pool_addr = parse_address(&cfg.pool_address)?;

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
