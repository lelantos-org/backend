//! The RPC metadata sweep: `assets.decimals`, `assets.symbol` and
//! `asset_yield.vault_name`.
//!
//! Runs beside the event path rather than in it. `AssetRegistered` carries none
//! of these columns and `YieldAssetAdded` carries only the venue, and an inline
//! RPC read would let a flaky endpoint stall event consumption or drop the
//! values permanently. Sweeping retries a failed read on the next tick and
//! repairs rows that predate these columns.
//!
//! Each column is fetched only when absent and written only when resolved, so a
//! token whose `symbol()` reverts still gets its decimals, and neither read can
//! clear the other's stored value.

use super::tick::ConsumeCtx;
use crate::adapters::DynTokenMetadata;
use crate::domain::address;
use crate::repositories::{asset_yield, assets};
use futures::stream::{self, StreamExt};
use std::future::Future;
use tracing::{debug, warn};

/// How many assets one tick tries to resolve. The registry is small and grows
/// only on `AssetRegistered`, so this throttles retry storms while an RPC is
/// down rather than serving as a paging mechanism.
const METADATA_PER_TICK: i64 = 16;

/// How many assets are resolved at once.
///
/// The sweep used to run its assets one after another, so a tick cost
/// `METADATA_PER_TICK` sequential RPC round trips and one endpoint at its
/// timeout stalled event consumption for the whole product. Bounded rather than
/// unbounded: the reads go to one node, and `METADATA_PER_TICK` simultaneous
/// `eth_call`s is what a public endpoint rate-limits.
const METADATA_CONCURRENCY: usize = 4;

/// Fill in whatever this chain's RPC can still resolve.
pub async fn fill_missing(ctx: &ConsumeCtx, chain_id: i64) {
    let Some(rpc) = ctx.token_meta.get(&chain_id) else {
        return;
    };
    // Together rather than in turn: both run inside the tick, so against a hung
    // node their timeouts would otherwise add up on the consume loop.
    tokio::join!(
        fill_token_metadata(ctx, rpc, chain_id),
        fill_vault_names(ctx, rpc, chain_id),
    );
}

/// Run the pending reads, dropping the ones that resolved nothing.
///
/// Bounded fan-out over the reads; the callers' writes stay sequential. The pool
/// this chain shares with the event path holds a handful of connections, so
/// fanning `UPDATE`s out too would contend with consume for them to save
/// nothing — the writes are single-row and local, and the RPC was the cost.
///
/// Takes the futures already built rather than a closure the stream calls: a
/// closure returning an `async move` block that borrows its captures leaves the
/// future's `Send` bound tied to an inferred lifetime, which `#[async_trait]`'s
/// boxed `Send` future then cannot prove.
async fn resolve_all<F, T>(reads: Vec<F>) -> Vec<T>
where
    F: Future<Output = Option<T>>,
{
    stream::iter(reads)
        .buffer_unordered(METADATA_CONCURRENCY)
        .filter_map(std::future::ready)
        .collect()
        .await
}

/// Resolve `fut` only when `wanted`, so both calls can sit in one `join!`.
///
/// Building the future does not start the call; nothing is sent for the arm that
/// is not wanted.
async fn fetch_if<T>(wanted: bool, fut: impl Future<Output = T>) -> Option<T> {
    if wanted { Some(fut.await) } else { None }
}

/// Resolve one asset's missing columns, returning what to store.
///
/// `None` when there is nothing to write, so the caller does not issue an empty
/// `UPDATE`. Reads only; the write is the caller's, which keeps the concurrent
/// section free of pool checkouts.
async fn resolve_metadata(
    rpc: &DynTokenMetadata,
    chain_id: i64,
    row: &assets::PendingMetadata,
) -> Option<(i64, assets::AssetMetadata)> {
    let asset_id_u64 = row.asset_id_u64;
    let Some(token) = address::from_column(&row.token) else {
        warn!(chain_id, asset_id_u64, "token is not a 20-byte address");
        return None;
    };
    let mut meta = assets::AssetMetadata::default();

    // Two independent `eth_call`s, issued together rather than one round trip
    // after the other. `None` is a column that already has a value.
    let (decimals, symbol) = tokio::join!(
        fetch_if(row.decimals.is_none(), rpc.decimals(token)),
        fetch_if(row.symbol.is_none(), rpc.symbol(token)),
    );
    match decimals {
        Some(Ok(d)) => meta.decimals = Some(i16::from(d)),
        // Left NULL and retried next tick rather than defaulted; assuming 18
        // would misreport every amount.
        Some(Err(e)) => warn!(chain_id, asset_id_u64, "decimals() failed: {}", e),
        None => {}
    }
    match symbol {
        Some(Ok(s)) => meta.symbol = Some(s),
        Some(Err(e)) => warn!(chain_id, asset_id_u64, "symbol() failed: {}", e),
        None => {}
    }

    (!meta.is_empty()).then_some((asset_id_u64, meta))
}

async fn fill_token_metadata(ctx: &ConsumeCtx, rpc: &DynTokenMetadata, chain_id: i64) {
    let pending = match assets::missing_metadata(&ctx.pool, chain_id, METADATA_PER_TICK).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(chain_id, "metadata backfill query failed: {}", e);
            return;
        }
    };

    let resolved = resolve_all(
        pending
            .iter()
            .map(|row| resolve_metadata(rpc, chain_id, row))
            .collect(),
    )
    .await;

    for (asset_id_u64, meta) in resolved {
        if let Err(e) = assets::set_metadata(&ctx.pool, chain_id, asset_id_u64, meta).await {
            warn!(
                chain_id,
                asset_id_u64, "storing asset metadata failed: {}", e
            );
        } else {
            debug!(chain_id, asset_id_u64, "asset metadata resolved");
        }
    }
}

/// Read one yield asset's vault name, returning what to store.
async fn resolve_vault_name(
    rpc: &DynTokenMetadata,
    chain_id: i64,
    row: &asset_yield::YieldAssetRef,
) -> Option<(i64, String)> {
    let asset_id_u64 = row.asset_id_u64;
    let Some(venue) = address::from_column(&row.venue) else {
        warn!(chain_id, asset_id_u64, "venue is not a 20-byte address");
        return None;
    };
    // A failure is left NULL and retried next tick, like a token without `symbol()`.
    rpc.vault_name(venue)
        .await
        .inspect_err(|e| warn!(chain_id, asset_id_u64, "vault name read failed: {}", e))
        .ok()
        .map(|name| (asset_id_u64, name))
}

/// Fill in `asset_yield.vault_name` for yield assets that do not have it yet.
async fn fill_vault_names(ctx: &ConsumeCtx, rpc: &DynTokenMetadata, chain_id: i64) {
    let pending =
        match asset_yield::missing_vault_name(&ctx.pool, chain_id, METADATA_PER_TICK).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(chain_id, "vault name backfill query failed: {}", e);
                return;
            }
        };

    let resolved = resolve_all(
        pending
            .iter()
            .map(|row| resolve_vault_name(rpc, chain_id, row))
            .collect(),
    )
    .await;

    for (asset_id_u64, name) in resolved {
        if let Err(e) = asset_yield::set_vault_name(&ctx.pool, chain_id, asset_id_u64, &name).await
        {
            warn!(chain_id, asset_id_u64, "storing vault name failed: {}", e);
        } else {
            debug!(chain_id, asset_id_u64, "vault name resolved");
        }
    }
}
