//! Per-endpoint response caches.
//!
//! One cache per endpoint rather than one shared map, so each carries its own
//! key shape, capacity and TTL. Two TTLs are in play: the configured analytic
//! one, and a fixed short one for the endpoints that track the head of the
//! chain.

use crate::domain::responses::{
    AnonymitySetOut, AssetOut, ChainFlowOut, ChainLockedOut, CountPoint, FlowPoint, KindCounts,
    PoolNotesOut, TreeAdvanceOut, TxKind, TxOut, YieldAssetOut,
};
use shared::cache::Cache;
use shared::cache::build;
use std::sync::Arc;
use std::time::Duration;

pub type AssetsKey = Option<i64>;
pub type AssetFlowsKey = (Option<i64>, Option<i64>, i64, Option<i64>);
pub type TreeAdvancesKey = (Option<i64>, Option<i64>, i64);
pub type TxCountsKey = (Option<i64>, i64, Option<i64>);
pub type ChainFlows24hKey = i64;
pub type LockedKey = Option<i64>;
pub type TransactionsKey = (Option<i64>, Option<i64>, Option<TxKind>, i64);
pub type TxKindsKey = (Option<i64>, i64, Option<i64>);
pub type AnonymitySetKey = (Option<i64>, Option<i64>, i64, i64);
pub type PoolNotesKey = Option<i64>;
pub type YieldKey = Option<i64>;

#[derive(Clone)]
pub struct AppCache {
    pub assets: Cache<AssetsKey, Arc<Vec<AssetOut>>>,
    pub asset_flows: Cache<AssetFlowsKey, Arc<Vec<FlowPoint>>>,
    pub tree_advances: Cache<TreeAdvancesKey, Arc<Vec<TreeAdvanceOut>>>,
    pub tx_counts: Cache<TxCountsKey, Arc<Vec<CountPoint>>>,
    pub chain_flows_24h: Cache<ChainFlows24hKey, Arc<Vec<ChainFlowOut>>>,
    /// All-time escrow balances. Analytic TTL, since they move with the flows
    /// the same views are built from.
    pub locked: Cache<LockedKey, Arc<Vec<ChainLockedOut>>>,
    /// Classified feed. Tracks the head of the chain, so it uses the short TTL
    /// rather than the analytic one.
    pub transactions: Cache<TransactionsKey, Arc<Vec<TxOut>>>,
    pub tx_kinds: Cache<TxKindsKey, Arc<Vec<KindCounts>>>,
    /// Denomination cohorts. Analytic TTL: the counts are all-time, so one more
    /// withdrawal moves a k that is already in the hundreds by one.
    pub anonymity_set: Cache<AnonymitySetKey, Arc<Vec<AnonymitySetOut>>>,
    /// Per-chain tree occupancy. Analytic TTL, like the other all-time figures.
    pub pool_notes: Cache<PoolNotesKey, Arc<Vec<PoolNotesOut>>>,
    /// Yield-bearing assets. Analytic TTL even though the underlying row is
    /// repolled on its own tick: serving a reading a few seconds old is what
    /// every other figure here does, and `updatedAt` carries the real age.
    pub asset_yield: Cache<YieldKey, Arc<Vec<YieldAssetOut>>>,
}

impl AppCache {
    /// `ttl_s` is the analytic-endpoint TTL from `ExplorerWebserverConfig`. The
    /// paginated `tree_advances` list uses a fixed short TTL because it tracks
    /// the head of the chain. Prices are not here: `PriceService` owns its own
    /// cache, on its own longer TTL, since a miss there costs an upstream
    /// round-trip rather than a query.
    pub fn new(ttl_s: u64) -> Self {
        let analytic = Duration::from_secs(ttl_s.max(1));
        let head = Duration::from_secs(5);
        Self {
            assets: build(64, analytic),
            asset_flows: build(2_048, analytic),
            tree_advances: build(2_048, head),
            tx_counts: build(2_048, analytic),
            chain_flows_24h: build(8, analytic),
            locked: build(32, analytic),
            transactions: build(512, head),
            tx_kinds: build(2_048, analytic),
            anonymity_set: build(512, analytic),
            pool_notes: build(32, analytic),
            asset_yield: build(32, analytic),
        }
    }
}
