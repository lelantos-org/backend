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
use crate::domain::models::BlockCursor;
use crate::repositories::{AtomicWriteRepo, BlockHashRepo};
use alloy::primitives::B256;
use std::sync::Arc;
use tracing::{info, warn};

/// A block height paired with the hash recorded for it.
pub type Checkpoint = (i64, Vec<u8>);

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
        if hash_matches(chain_hash, &anchor.1) {
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
        let anchor_block = anchor.0;
        let limit = anchor_block.saturating_sub(max_depth as i64).max(floor);

        // Starts below the anchor: it is the block that just failed.
        let below = if anchor_block > limit {
            self.raw_events
                .block_hashes_desc(chain_id, limit, anchor_block - 1)
                .await?
        } else {
            Vec::new()
        };

        for (block, stored) in below {
            if !still_canonical(rpc, block, &stored).await? {
                continue;
            }
            warn!(
                chain_id,
                anchor_block,
                survived = block,
                "chain diverged above block {block}"
            );
            return Ok(Divergence {
                rewind_to: block + 1,
                anchor: Some((block, stored)),
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
        let (last_block, last_block_hash) =
            divergence.anchor.clone().unwrap_or((new_scan, Vec::new()));

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
                    last_block,
                    last_block_hash,
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
    Some((cursor.last_block, cursor.last_block_hash.clone()))
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
mod tests {
    use super::*;
    use crate::adapters::rpc::{BlockMeta, ChainRpc};
    use crate::domain::models::RawEvent;
    use alloy::primitives::{Address, B256};
    use alloy::rpc::types::eth::Log;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn hash(byte: u8) -> Vec<u8> {
        vec![byte; 32]
    }

    /// A synthetic chain: block height → hash.
    struct FakeChain(HashMap<u64, B256>);

    impl FakeChain {
        /// Blocks `range`, each hashed from `tag` so two chains built with
        /// different tags diverge at every height.
        fn new(range: std::ops::RangeInclusive<i64>, tag: u8) -> Arc<Self> {
            Arc::new(Self(
                range
                    .map(|n| (n as u64, B256::repeat_byte(tag ^ (n as u8))))
                    .collect(),
            ))
        }
    }

    #[async_trait]
    impl ChainRpc for FakeChain {
        async fn tip(&self) -> Result<u64, IngesterError> {
            Ok(self.0.keys().copied().max().unwrap_or(0))
        }
        async fn fetch_logs(
            &self,
            _a: Address,
            _f: u64,
            _t: u64,
        ) -> Result<Vec<Log>, IngesterError> {
            Ok(Vec::new())
        }
        async fn fetch_block_meta(
            &self,
            _b: &[u64],
        ) -> Result<HashMap<u64, BlockMeta>, IngesterError> {
            Ok(HashMap::new())
        }
        async fn block_hash_at(&self, n: u64) -> Result<Option<B256>, IngesterError> {
            Ok(self.0.get(&n).copied())
        }
    }

    /// In-memory stand-ins for the three repositories.
    #[derive(Default)]
    struct FakeStore {
        cursor: Mutex<Option<BlockCursor>>,
        /// block → hash, as if read back out of `raw_events`.
        hashes: Mutex<Vec<Checkpoint>>,
        rewound: Mutex<Vec<(i64, i64)>>,
        /// Times `block_hashes_desc` was issued. The happy path must not.
        hash_queries: AtomicUsize,
    }

    impl FakeStore {
        /// Seed as though the ingester had committed `range` on chain `tag`.
        fn seeded(range: std::ops::RangeInclusive<i64>, tag: u8) -> Arc<Self> {
            let top = *range.end();
            let store = Self::default();
            *store.hashes.lock().unwrap() = range
                .clone()
                .rev()
                .map(|n| (n, B256::repeat_byte(tag ^ (n as u8)).0.to_vec()))
                .collect();
            *store.cursor.lock().unwrap() = Some(BlockCursor {
                chain_id: 1,
                last_block: top,
                last_block_hash: B256::repeat_byte(tag ^ (top as u8)).0.to_vec(),
                last_scanned_block: top,
            });
            Arc::new(store)
        }

        fn with_cursor(cursor: Option<BlockCursor>) -> Arc<Self> {
            let store = Self::default();
            *store.cursor.lock().unwrap() = cursor;
            Arc::new(store)
        }
    }

    #[async_trait]
    impl BlockHashRepo for FakeStore {
        async fn block_hashes_desc(
            &self,
            _chain_id: i64,
            from_block: i64,
            to_block: i64,
        ) -> Result<Vec<Checkpoint>, IngesterError> {
            self.hash_queries.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .hashes
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| *n >= from_block && *n <= to_block)
                .cloned()
                .collect())
        }
    }

    #[async_trait]
    impl AtomicWriteRepo for FakeStore {
        async fn commit_batch(
            &self,
            _rows: &[RawEvent],
            _cursor: &BlockCursor,
        ) -> Result<usize, IngesterError> {
            Ok(0)
        }
        async fn rewind(
            &self,
            chain_id: i64,
            from_block: i64,
            cursor: &BlockCursor,
        ) -> Result<usize, IngesterError> {
            self.rewound.lock().unwrap().push((chain_id, from_block));
            *self.cursor.lock().unwrap() = Some(cursor.clone());
            Ok(0)
        }
    }

    fn service(store: &Arc<FakeStore>) -> ReorgService {
        ReorgService::new(store.clone(), store.clone())
    }

    /// Drive `check_anchor` the way the live tick does: read the cursor, derive
    /// the anchor, ask the chain for its hash, then check.
    ///
    /// Kept in the tests rather than as a production convenience so there is
    /// exactly one entry point to the anchor check, and it is the one the tick
    /// executes.
    async fn check(store: &Arc<FakeStore>, chain: &DynRpc, max_depth: u64) -> Option<Divergence> {
        let cursor = store.cursor.lock().unwrap().clone();
        let anchor = cursor.as_ref().and_then(anchor_of)?;
        let chain_hash = chain.block_hash_at(anchor.0 as u64).await.unwrap();
        service(store)
            .check_anchor(1, chain, FLOOR, max_depth, &anchor, chain_hash)
            .await
            .unwrap()
    }

    const DEPTH: u64 = 32;
    const FLOOR: i64 = 100;

    #[tokio::test]
    async fn an_untouched_chain_reports_no_divergence() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        let chain = FakeChain::new(100..=110, 0xa0) as DynRpc;
        let got = check(&store, &chain, DEPTH).await;
        assert!(got.is_none(), "same hashes, no reorg");
    }

    /// The anchor walk reads `raw_events` with a `DISTINCT ON` over the last
    /// `reorg_depth` blocks. Issuing it when the anchor still matches scans
    /// thousands of rows per tick and discards every one of them.
    #[tokio::test]
    async fn a_matching_anchor_does_not_query_the_stored_hashes() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        let chain = FakeChain::new(100..=110, 0xa0) as DynRpc;

        check(&store, &chain, DEPTH).await;

        assert_eq!(
            store.hash_queries.load(Ordering::SeqCst),
            0,
            "the happy path must not touch raw_events"
        );
    }

    /// The mirror of the above: once the anchor fails, the walk is the only way
    /// to bound the fork, so the query must fire.
    #[tokio::test]
    async fn a_diverged_anchor_does_query_the_stored_hashes() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        let chain = FakeChain::new(100..=110, 0xff) as DynRpc;

        check(&store, &chain, DEPTH).await.expect("fork detected");

        assert_eq!(store.hash_queries.load(Ordering::SeqCst), 1);
    }

    /// The chain replaced the top few blocks. Rewinding must target the lowest
    /// diverged block rather than the first one noticed.
    #[tokio::test]
    async fn rewinds_to_the_lowest_diverged_block() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        // 100..=107 unchanged; 108..=110 replaced.
        let mut blocks: HashMap<u64, B256> = (100..=107)
            .map(|n| (n as u64, B256::repeat_byte(0xa0 ^ (n as u8))))
            .collect();
        blocks.extend((108..=110).map(|n| (n as u64, B256::repeat_byte(0xff ^ (n as u8)))));
        let chain = Arc::new(FakeChain(blocks)) as DynRpc;

        let divergence = check(&store, &chain, DEPTH).await.expect("fork detected");

        assert_eq!(divergence.rewind_to, 108);
        assert_eq!(divergence.anchor.expect("survivor").0, 107);
    }

    /// A chain that has committed nothing has no anchor to check. Treating the
    /// seeded empty hash as one would compare against zero bytes and report a
    /// divergence on every tick.
    #[tokio::test]
    async fn an_empty_anchor_is_not_a_divergence() {
        let store = FakeStore::with_cursor(Some(BlockCursor {
            chain_id: 1,
            last_block: 0,
            last_block_hash: Vec::new(),
            last_scanned_block: 500,
        }));
        let chain = FakeChain::new(100..=110, 0xa0) as DynRpc;
        assert!(check(&store, &chain, DEPTH).await.is_none());
    }

    #[tokio::test]
    async fn a_chain_with_no_cursor_is_not_a_divergence() {
        let store = FakeStore::with_cursor(None);
        let chain = FakeChain::new(100..=110, 0xa0) as DynRpc;
        assert!(check(&store, &chain, DEPTH).await.is_none());
    }

    /// Deeper than `reorg_depth`: nothing in the window survives, so the whole
    /// window is discarded and no anchor remains to record.
    #[tokio::test]
    async fn a_fork_deeper_than_the_window_discards_the_whole_window() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        let chain = FakeChain::new(100..=110, 0xff) as DynRpc;

        let divergence = check(&store, &chain, 4).await.expect("fork detected");

        assert_eq!(divergence.rewind_to, 106, "anchor 110 minus depth 4");
        assert!(divergence.anchor.is_none(), "nothing survived to anchor on");
    }

    /// The walk must never propose discarding blocks below `start_block`, which
    /// the ingester never claimed to know.
    #[tokio::test]
    async fn the_walk_stops_at_the_configured_floor() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        let chain = FakeChain::new(100..=110, 0xff) as DynRpc;

        let divergence = check(&store, &chain, 1_000).await.expect("fork detected");

        assert_eq!(divergence.rewind_to, FLOOR);
    }

    /// A pruned or unavailable block is not a survivor. Treating a `None` hash as
    /// a match would anchor onto a block the node cannot produce.
    #[tokio::test]
    async fn a_block_the_node_no_longer_has_is_not_canonical() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        // Only 100..=105 remain visible; the anchor at 110 is gone.
        let chain = FakeChain::new(100..=105, 0xa0) as DynRpc;

        let divergence = check(&store, &chain, DEPTH)
            .await
            .expect("missing anchor is a divergence");

        assert_eq!(divergence.rewind_to, 106);
    }

    #[tokio::test]
    async fn rewind_resets_the_cursor_to_the_surviving_anchor() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        let divergence = Divergence {
            rewind_to: 108,
            anchor: Some((107, hash(0x07))),
        };

        service(&store).rewind(1, &divergence).await.unwrap();

        assert_eq!(*store.rewound.lock().unwrap(), vec![(1, 108)]);
        let cursor = store
            .cursor
            .lock()
            .unwrap()
            .clone()
            .expect("cursor written");
        assert_eq!(cursor.last_block, 107);
        assert_eq!(cursor.last_block_hash, hash(0x07));
        assert_eq!(cursor.last_scanned_block, 107, "rescan starts at 108");
    }

    /// With no survivor there is no hash to record, and the next tick must see
    /// an empty anchor rather than a stale one.
    #[tokio::test]
    async fn rewind_without_a_survivor_clears_the_anchor() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        let divergence = Divergence {
            rewind_to: 100,
            anchor: None,
        };

        service(&store).rewind(1, &divergence).await.unwrap();

        let cursor = store
            .cursor
            .lock()
            .unwrap()
            .clone()
            .expect("cursor written");
        assert!(cursor.last_block_hash.is_empty());
        assert_eq!(cursor.last_scanned_block, 99);
    }
}
