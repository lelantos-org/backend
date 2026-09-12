//! The periodic sweep: reclaim what nothing else reclaims, and publish the
//! occupancy of every bound this service relies on.
//!
//! Both halves exist for the same reason. The rate limiter's keyed stores are
//! the one structure here that grows with what an unauthenticated caller sends
//! and never shrinks on its own, and the caches are bounded by a byte budget
//! whose right value is only knowable by watching it. A bound nobody can see is
//! a bound nobody will notice failing.

use crate::app::AppState;
use shared::metrics::name;
use std::time::Duration;

/// How often the sweep runs. Neither half is urgent: an idle bucket costs a few
/// dozen bytes, and the gauges are sampled occupancy a scrape would smooth
/// anyway.
const INTERVAL: Duration = Duration::from_secs(60);

/// Run the sweep until the process exits.
///
/// Spawned rather than awaited: it is not part of serving a request, and
/// nothing should wait on it.
pub async fn run(state: AppState) {
    let mut ticker = tokio::time::interval(INTERVAL);
    // The first tick fires immediately; skipping the catch-up burst keeps a
    // delayed runtime from running several sweeps back to back.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;

        // `governor` retires no key on its own; without this every distinct
        // client address seen since start stays in the dashmap for the life of
        // the process.
        state.client_limiter.gc();

        for (bucket, count) in state.client_limiter.key_counts() {
            metrics::gauge!(name::RPC_PROXY_RATELIMIT_KEYS, "bucket" => bucket).set(count as f64);
        }

        for (chain_id, chain) in state.chains.iter() {
            let label = chain_id.to_string();
            for (class, entries) in chain.caches().entry_counts() {
                metrics::gauge!(
                    name::RPC_PROXY_CACHE_ENTRIES,
                    "chain" => label.clone(),
                    "class" => class.label(),
                )
                .set(entries as f64);
            }
            metrics::gauge!(name::RPC_PROXY_UPSTREAM_PERMITS, "chain" => label)
                .set(chain.upstream_permits_available() as f64);
        }
    }
}
