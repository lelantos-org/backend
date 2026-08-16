use crate::adapters::{TokenKey, TokenPrice};
use crate::domain::responses::{
    AssetOut, ChainFlowOut, CountPoint, FlowPoint, KindCounts, TreeAdvanceOut, TxOut,
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
pub type TransactionsKey = (Option<i64>, Option<i64>, i64);
pub type TxKindsKey = (Option<i64>, i64, Option<i64>);

#[derive(Clone)]
pub struct AppCache {
    pub assets: Cache<AssetsKey, Arc<Vec<AssetOut>>>,
    pub asset_flows: Cache<AssetFlowsKey, Arc<Vec<FlowPoint>>>,
    pub tree_advances: Cache<TreeAdvancesKey, Arc<Vec<TreeAdvanceOut>>>,
    pub tx_counts: Cache<TxCountsKey, Arc<Vec<CountPoint>>>,
    pub chain_flows_24h: Cache<ChainFlows24hKey, Arc<Vec<ChainFlowOut>>>,
    /// Classified feed. Tracks the head of the chain, so it shares the short
    /// TTL rather than the analytic one.
    pub transactions: Cache<TransactionsKey, Arc<Vec<TxOut>>>,
    pub tx_kinds: Cache<TxKindsKey, Arc<Vec<KindCounts>>>,
    /// `None` records a token the provider could not price. Caching that
    /// answer is the point: without it every request re-asks upstream about
    /// tokens that will never have a price.
    pub prices: Cache<TokenKey, Option<TokenPrice>>,
}

impl AppCache {
    /// `ttl_s` is the analytic-endpoint TTL from `ExplorerWebserverConfig`.
    /// `tree_advances` paginated list uses a fixed short TTL because it
    /// tracks the head of the chain. `price_ttl_s` is longer than either:
    /// prices move slowly and every miss costs an upstream round-trip.
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
            transactions: build(512, head),
            tx_kinds: build(2_048, analytic),
            prices: build(1_024, price),
        }
    }
}
