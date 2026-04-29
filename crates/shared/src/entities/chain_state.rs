use crate::chain::ChainId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainState {
    pub chain_id: ChainId,
    pub last_block: i64,
    pub last_block_hash: Vec<u8>,
    pub last_scanned_block: i64,
}
