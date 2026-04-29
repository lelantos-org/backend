use crate::chain::ChainId;
use serde::{Deserialize, Serialize};

/// One on-chain `RootAdvanced` event from `CommitmentTree._advanceRoot`.
/// Domain entity for the `tree_advances` table; written by explorer-indexer,
/// read by fmd-indexer for cm → leaf_index correlation and by the relayer +
/// fmd-webserver for path verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeAdvance {
    pub chain_id: ChainId,
    pub block_number: i64,
    pub log_index: i32,
    pub start_index: i64,
    pub inserted: i32,
    pub old_root: Vec<u8>,
    pub new_root: Vec<u8>,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}
