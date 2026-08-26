use crate::domain::error::IngesterError;
use alloy::primitives::Address;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct RawEvent {
    pub chain_id: i64,
    pub block_number: i64,
    /// What Solidity's `block.number` returned in this block. Equal to
    /// `block_number` except on Arbitrum, where the EVM reports the L1 height and
    /// MASP hashes that into the deposit digest.
    pub evm_block_number: i64,
    pub block_hash: Vec<u8>,
    pub block_ts: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub event_kind: i16,
    pub topics: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BlockCursor {
    pub chain_id: i64,
    /// Highest block whose hash was verified against the chain. Paired with
    /// `last_block_hash`, this is the anchor reorg detection walks back from, so
    /// it is only written alongside a real block.
    pub last_block: i64,
    pub last_block_hash: Vec<u8>,
    pub last_scanned_block: i64,
}

#[derive(Debug, Clone)]
pub enum TickOutcome {
    Idle,
    Empty {
        to: i64,
    },
    Committed {
        count: usize,
        to: i64,
    },
    Reorg {
        rewind_to: i64,
    },
    /// Too far behind for the live tail, so the worker returns to chunked,
    /// parallel backfill.
    Lagging {
        lag: i64,
    },
}

pub fn parse_address(s: &str) -> Result<Address, IngesterError> {
    Address::from_str(s).map_err(|e| IngesterError::Config(format!("pool_address: {}", e)))
}
