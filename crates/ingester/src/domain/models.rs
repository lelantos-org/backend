//! The types one tick reads, writes and reports.
//!
//! Pure data plus the parsing that produces it; no IO and no database, so the
//! layers above can share a shape without sharing a dependency.

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
    /// The emitting contract. One `eth_getLogs` covers the pool and the
    /// governor, and topic0 alone does not say which of them a log came from.
    pub address: Vec<u8>,
}

/// Per-block facts the ingester needs beyond the log itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockMeta {
    pub timestamp: u64,
    /// What Solidity's `block.number` returns inside this block.
    ///
    /// Equal to the block's own height on Ethereum and OP-stack chains. On
    /// Arbitrum it is the L1 height, which is what MASP hashes into the deposit
    /// digest; replaying the L2 height there reverts `DigestMismatch`. Taken from
    /// the block's non-standard `l1BlockNumber` field when the node reports one.
    pub evm_block_number: u64,
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

/// A block height paired with the hash recorded for it.
///
/// The unit the reorg check compares: a height on its own says nothing about
/// which branch it belongs to, and the two are never useful apart. Lives here
/// rather than beside the reorg service because
/// [`crate::repositories::BlockHashRepo`] reads these back out of `raw_events`,
/// and a repository may not depend on a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub block: i64,
    pub hash: Vec<u8>,
}

/// Where a chain's scan stands: the cursor's watermark, or one block below
/// `start_block` when nothing has been committed yet.
///
/// Both entry points size themselves from this — the live tail on its first
/// tick, and the catch-up when it measures the lag — so the rule lives in one
/// place. The `- 1` is load-bearing: the next block to scan is `watermark + 1`,
/// so a chain configured to begin at `start_block` must sit one below it.
pub fn scanned_watermark(cursor: Option<&BlockCursor>, start_block: i64) -> i64 {
    cursor.map_or(start_block - 1, |c| c.last_scanned_block)
}

/// What one live tick accomplished.
///
/// `Copy` and comparable like the workspace's other outcome enums
/// (`WorkerExit`, `shared::tick::TickProgress`): every field is a plain integer,
/// and the handler pacing the loop needs to both test and forward the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    Idle,
    Empty {
        to: i64,
        /// Whether `to` was the tip the tick observed. A scan capped short of
        /// the tip leaves known work behind; one that reached it does not.
        reached_tip: bool,
    },
    Committed {
        count: usize,
        to: i64,
        reached_tip: bool,
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
