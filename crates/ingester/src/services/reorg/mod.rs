//! Reorg detection and rewind.
//!
//! Detection is anchor-based and looks backwards. Comparing incoming logs
//! against stored hashes for the same blocks cannot detect anything: a tick only
//! fetches blocks above `last_scanned_block`, and committing raises
//! `last_scanned_block` to the top of the scanned range, so every incoming block
//! number is above anything in `raw_events` and the stored lookup always misses.
//!
//! Instead, the cursor records the highest block whose hash was verified, and a
//! reorg is the case where the chain no longer reports that hash at that height.
//! From there the walk descends through the stored hashes until one still
//! matches, and rewinds to just above it.

use crate::adapters::DynRpc;
use crate::domain::error::IngesterError;
use crate::domain::models::{BlockCursor, Checkpoint};
use crate::repositories::{AtomicWriteRepo, BlockHashRepo};
use alloy::primitives::B256;
use std::sync::Arc;
use tracing::{info, warn};

/// A confirmed fork: what to discard, and the last block known to survive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// First block that is no longer canonical. Everything at or above it is
    /// discarded and re-derived from the chain.
    pub rewind_to: i64,
    /// Highest block still agreeing with the chain, with its hash. `None` when the
    /// whole search window diverged, leaving no verified anchor to record.
    pub anchor: Option<Checkpoint>,
}

pub struct ReorgService {
    writes: Arc<dyn AtomicWriteRepo>,
    raw_events: Arc<dyn BlockHashRepo>,
}

impl ReorgService {
    pub fn new(writes: Arc<dyn AtomicWriteRepo>, raw_events: Arc<dyn BlockHashRepo>) -> Self {
        Self { writes, raw_events }
    }

    /// Check that the stored anchor is still on the canonical chain.
    ///
    /// Takes the anchor and the chain's hash at that height rather than reading
    /// them: the live tick fetches both already, overlapping the anchor lookup
    /// with the tip lookup, and re-reading here would undo that.
    ///
    /// Costs nothing beyond the comparison in the common case. Only a mismatch
    /// walks back, at most `max_depth` blocks. `floor` is `start_block`: the
    /// ingester claims no knowledge below it, so the walk stops there.
    pub async fn check_anchor(
        &self,
        chain_id: i64,
        rpc: &DynRpc,
        floor: i64,
        max_depth: u64,
        anchor: &Checkpoint,
        chain_hash: Option<B256>,
    ) -> Result<Option<Divergence>, IngesterError> {
        if hash_matches(chain_hash, &anchor.hash) {
            return Ok(None);
        }
        self.locate_fork(chain_id, rpc, floor, max_depth, anchor)
            .await
            .map(Some)
    }

    /// Walk back from a known-bad anchor to the highest block the chain still
    /// agrees with.
    ///
    /// Reached only once the anchor has already failed, which is what keeps
    /// `block_hashes_desc` off the common path: it is a `DISTINCT ON` over the
    /// last `max_depth` blocks of `raw_events`, and running it on every tick
    /// scans thousands of rows to discard all of them.
    async fn locate_fork(
        &self,
        chain_id: i64,
        rpc: &DynRpc,
        floor: i64,
        max_depth: u64,
        anchor: &Checkpoint,
    ) -> Result<Divergence, IngesterError> {
        let anchor_block = anchor.block;
        let limit = anchor_block.saturating_sub(max_depth as i64).max(floor);

        // Starts below the anchor: it is the block that just failed.
        let below = if anchor_block > limit {
            self.raw_events
                .block_hashes_desc(chain_id, limit, anchor_block - 1)
                .await?
        } else {
            Vec::new()
        };

        for stored in below {
            if !still_canonical(rpc, stored.block, &stored.hash).await? {
                continue;
            }
            let survived = stored.block;
            warn!(
                chain_id,
                anchor_block, survived, "chain diverged above block {survived}"
            );
            return Ok(Divergence {
                rewind_to: survived + 1,
                anchor: Some(stored),
            });
        }

        // Nothing in the window survives, so discard it all and re-derive. The
        // walk is bounded by `max_depth`, so a deeper fork requires a manual
        // cursor reset rather than an unbounded backwards scan.
        warn!(
            chain_id,
            anchor_block, limit, "no verified block within reorg_depth; rewinding whole window"
        );
        Ok(Divergence {
            rewind_to: limit,
            anchor: None,
        })
    }

    /// Discard the diverged suffix and reset the cursor to the surviving
    /// anchor, atomically.
    pub async fn rewind(
        &self,
        chain_id: i64,
        divergence: &Divergence,
    ) -> Result<usize, IngesterError> {
        let new_scan = (divergence.rewind_to - 1).max(0);
        let survivor = divergence.anchor.clone().unwrap_or(Checkpoint {
            block: new_scan,
            hash: Vec::new(),
        });

        info!(
            chain_id,
            rewind_to = divergence.rewind_to,
            new_scan,
            "rewinding chain state"
        );
        // Consumers stream `raw_events` by ascending id and re-read the
        // replacement rows on their own, but state derived from the deleted rows
        // is invisible to that cursor. `chain_reorgs` is the durable record and
        // the NOTIFY only reduces latency; both are written by the same
        // transaction as the delete, so neither can describe a rewind that did
        // not happen.
        let deleted = self
            .writes
            .rewind(
                chain_id,
                divergence.rewind_to,
                &BlockCursor {
                    chain_id,
                    last_block: survivor.block,
                    last_block_hash: survivor.hash,
                    last_scanned_block: new_scan,
                },
            )
            .await?;
        info!(chain_id, deleted, new_scan, "rewind applied");
        Ok(deleted)
    }
}

/// The cursor's verified anchor, if it has one.
///
/// An empty hash means no verified block yet: a fresh chain, or one whose
/// scanned range has produced no logs. It is not an anchor, and treating it as
/// one would compare against bytes that are not a hash.
///
/// Free and pure so the live tick can derive the anchor from a cursor it already
/// holds, without a second read.
pub fn anchor_of(cursor: &BlockCursor) -> Option<Checkpoint> {
    if cursor.last_block_hash.is_empty() {
        return None;
    }
    Some(Checkpoint {
        block: cursor.last_block,
        hash: cursor.last_block_hash.clone(),
    })
}

/// Does the chain's hash at some height match what was recorded for it?
///
/// A height the chain cannot produce (`None`) counts as a mismatch: the block
/// was almost certainly orphaned, and treating it as a match would anchor onto a
/// block the node cannot serve.
fn hash_matches(chain: Option<B256>, stored: &[u8]) -> bool {
    chain.is_some_and(|h| h.0.as_slice() == stored)
}

/// Does the chain still report `stored` as the hash at `block`?
async fn still_canonical(rpc: &DynRpc, block: i64, stored: &[u8]) -> Result<bool, IngesterError> {
    Ok(hash_matches(rpc.block_hash_at(block as u64).await?, stored))
}

#[cfg(test)]
mod tests;
