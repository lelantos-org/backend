use crate::adapters::{TokenKey, TokenPrice};
use crate::domain::responses::{
    AnonymitySetOut, AssetOut, ChainFlowOut, ChainLockedOut, CountPoint, FlowPoint, KindCounts,
    PoolNotesOut, TreeAdvanceOut, TxKind, TxOut,
};
use moka::future::Cache;
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
    /// `None` records a token the provider could not price. Caching that answer
    /// stops every request from re-asking upstream about tokens that will never
    /// have a price.
    pub prices: Cache<TokenKey, Option<TokenPrice>>,
}

impl AppCache {
    /// `ttl_s` is the analytic-endpoint TTL from `ExplorerWebserverConfig`. The
    /// paginated `tree_advances` list uses a fixed short TTL because it tracks
    /// the head of the chain. `price_ttl_s` is longer than either: prices move
    /// slowly and every miss costs an upstream round-trip.
    pub fn new(ttl_s: u64, price_ttl_s: u64) -> Self {
        let analytic = Duration::from_secs(ttl_s.max(1));
        let head = Duration::from_secs(5);
        let price = Duration::from_secs(price_ttl_s.max(1));
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
            prices: build(1_024, price),
        }
    }
}
