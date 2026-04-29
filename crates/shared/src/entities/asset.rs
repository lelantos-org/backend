use crate::chain::ChainId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub chain_id: ChainId,
    pub address: Vec<u8>,
    pub asset_id: Vec<u8>,
    pub name: String,
    pub symbol: String,
    pub decimals: i16,
    pub metadata_pending: bool,
}
