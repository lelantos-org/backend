//! The venue-APY tick loop and its per-chain election.
//!
//! The only part of this service that writes. [`crate::services::venue_apy`]
//! does the measuring; this decides who is allowed to, and how often.

use crate::adapters::rpc::{RpcEndpoint, endpoint};
use crate::app::config::RegistryConfig;
use crate::services::venue_apy::VenueApyWorker;
use asset_registry::AssetRegistry;
use database::DbPool;
use database::advisory::{self, ChainLock};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// How often the worker re-measures. A week-long window does not move in an
/// hour, and each pass costs archive reads.
const REFRESH: Duration = Duration::from_secs(30 * 60);

/// Start one worker per configured chain that has an endpoint to measure with.
///
/// A chain without one still serves every route; its catalog simply carries no
/// rate. Neither a missing endpoint nor a malformed one is fatal, for the same
/// reason: the registry is the product here, and the estimate is a badge on it.
pub fn spawn_all(cfg: &RegistryConfig, pool: DbPool, assets: Arc<AssetRegistry>) {
    for c in &cfg.chains {
        // `apy_rpc_url` first because it is the one allowed to be slow and
        // privileged; `rpc_url` is the browser-facing endpoint and only stands
        // in for it when no separate archive endpoint was configured.
        let Some(url) = c.apy_rpc_url.as_deref().or(c.rpc_url.as_deref()) else {
            info!(
                chain_id = c.chain_id,
                "venue apy: no rpc configured, not measuring"
            );
            continue;
        };
        match endpoint(url) {
            Ok(rpc) => spawn(
                c.chain_id,
                cfg.database_url.clone(),
                pool.clone(),
                &rpc,
                assets.clone(),
            ),
            Err(e) => warn!(chain_id = c.chain_id, error = %e, "venue apy: worker not started"),
        }
    }
}

/// Drive one chain's estimates on a fixed interval, on one replica.
///
/// Ticks are skipped rather than queued when one runs long. There is no fatal
/// case: every failure here means one badge renders without a figure, so the
/// worker logs and waits for the next tick.
///
/// The rows come through the same cached registry the routes use; a measurement
/// runs twice an hour, so it adds no meaningful load.
///
/// # Why this is elected
///
/// This service is otherwise stateless and scales freely, but the measurement is
/// not: it writes `asset_yield_sample` and issues archive `eth_call`s. Running it
/// on every replica would lay down duplicate samples and multiply the archive
/// load by the replica count. A Postgres advisory lock elects one measurer per
/// chain; the rest serve the row it writes, which is why the estimate is stored
/// rather than cached in the process that computed it.
///
/// The lock is re-checked every tick, because a dropped connection would
/// otherwise leave a former leader measuring while a standby acquires the freed
/// lock. Failover costs at most one refresh interval — well inside the window
/// after which a stored estimate stops being published anyway.
pub fn spawn(
    chain_id: i64,
    database_url: String,
    pool: DbPool,
    rpc: &RpcEndpoint,
    assets: Arc<AssetRegistry>,
) {
    let mut worker = VenueApyWorker::new(chain_id, pool, rpc);
    let key = advisory::chain_key(advisory::NS_VENUE_APY, chain_id);
    tokio::spawn(async move {
        let mut lock: Option<ChainLock> = None;
        let mut tick = tokio::time::interval(REFRESH);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if !hold_leadership(&mut lock, &database_url, key, chain_id).await {
                continue;
            }
            match assets.for_chain(chain_id).await {
                Ok(rows) => worker.refresh(&rows).await,
                Err(e) => warn!(chain_id, error = %e, "venue apy: asset read failed"),
            }
        }
    });
}

/// Whether this replica may measure `chain_id` this tick.
///
/// Takes the lock if it is free, confirms it is still held if we have it, and
/// drops it the moment its session dies — a lock believed held over a dead
/// connection is the one state that would let two replicas measure at once.
async fn hold_leadership(
    lock: &mut Option<ChainLock>,
    database_url: &str,
    key: i64,
    chain_id: i64,
) -> bool {
    if let Some(held) = lock.as_mut() {
        if held.is_alive().await {
            return true;
        }
        warn!(chain_id, "venue apy: lock connection died, standing down");
        *lock = None;
        return false;
    }
    match ChainLock::try_acquire(database_url, key).await {
        Ok(Some(held)) => {
            info!(chain_id, "venue apy: measuring this chain");
            *lock = Some(held);
            true
        }
        // Another replica holds it. The standby path, not an error.
        Ok(None) => {
            debug!(chain_id, "venue apy: another replica is measuring");
            false
        }
        Err(e) => {
            warn!(chain_id, error = %e, "venue apy: lock unavailable");
            false
        }
    }
}
