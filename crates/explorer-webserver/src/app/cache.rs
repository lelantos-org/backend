use crate::domain::responses::{AssetOut, ChainFlowOut, CountPoint, FlowPoint, TreeAdvanceOut};
use moka::future::Cache;
use shared::cache::build;
use std::sync::Arc;
use std::time::Duration;

pub type AssetsKey = Option<i64>;
pub type AssetFlowsKey = (Option<i64>, Option<i64>, i64, Option<i64>);
pub type TreeAdvancesKey = (Option<i64>, Option<i64>, i64);
pub type TxCountsKey = (Option<i64>, i64, Option<i64>);
pub type ChainFlows24hKey = i64;

#[derive(Clone)]
pub struct AppCache {
    pub assets: Cache<AssetsKey, Arc<Vec<AssetOut>>>,
    pub asset_flows: Cache<AssetFlowsKey, Arc<Vec<FlowPoint>>>,
    pub tree_advances: Cache<TreeAdvancesKey, Arc<Vec<TreeAdvanceOut>>>,
    pub tx_counts: Cache<TxCountsKey, Arc<Vec<CountPoint>>>,
    pub chain_flows_24h: Cache<ChainFlows24hKey, Arc<Vec<ChainFlowOut>>>,
}

impl AppCache {
    /// `ttl_s` is the analytic-endpoint TTL from `ExplorerWebserverConfig`.
    /// `tree_advances` paginated list uses a fixed short TTL because it
    /// tracks the head of the chain.
    pub fn new(ttl_s: u64) -> Self {
        let analytic = Duration::from_secs(ttl_s.max(1));
        let head = Duration::from_secs(5);
        Self {
            assets: build(64, analytic),
            asset_flows: build(2_048, analytic),
            tree_advances: build(2_048, head),
            tx_counts: build(2_048, analytic),
            chain_flows_24h: build(8, analytic),
        }
    }
}
