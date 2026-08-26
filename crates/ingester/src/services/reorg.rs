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
use crate::repositories::{AtomicWriteRepo, ChainStateRepo, RawEventRepo};
use std::sync::Arc;
use tracing::{info, warn};

/// A block height paired with the hash recorded for it.
type Checkpoint = (i64, Vec<u8>);

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
    raw_events: Arc<dyn RawEventRepo>,
    chain_state: Arc<dyn ChainStateRepo>,
}

impl ReorgService {
    pub fn new(
        writes: Arc<dyn AtomicWriteRepo>,
        raw_events: Arc<dyn RawEventRepo>,
        chain_state: Arc<dyn ChainStateRepo>,
    ) -> Self {
        Self {
            writes,
            raw_events,
            chain_state,
        }
    }

    /// Check that the stored anchor is still on the canonical chain.
    ///
    /// Costs one `eth_getBlockByNumber` in the common case, and walks back at
    /// most `max_depth` blocks when that disagrees.
    ///
    /// `floor` is `start_block`: the ingester claims no knowledge below it, so
    /// the walk stops there.
    pub async fn check_anchor(
        &self,
        chain_id: i64,
        rpc: &DynRpc,
        floor: i64,
        max_depth: u64,
    ) -> Result<Option<Divergence>, IngesterError> {
        let Some(anchor) = self.stored_anchor(chain_id).await? else {
            return Ok(None);
        };
        let (anchor_block, _) = anchor;
        let limit = anchor_block.saturating_sub(max_depth as i64).max(floor);

        for (block, stored) in self.checkpoints(chain_id, anchor, limit).await? {
            if !still_canonical(rpc, block, &stored).await? {
                continue;
            }
            if block == anchor_block {
                return Ok(None);
            }
            warn!(
                chain_id,
                anchor_block,
                survived = block,
                "chain diverged above block {block}"
            );
            return Ok(Some(Divergence {
                rewind_to: block + 1,
                anchor: Some((block, stored)),
            }));
        }

        // Nothing in the window survives, so discard it all and re-derive. The
        // walk is bounded by `max_depth`, so a deeper fork requires a manual
        // cursor reset rather than an unbounded backwards scan.
        warn!(
            chain_id,
            anchor_block, limit, "no verified block within reorg_depth; rewinding whole window"
        );
        Ok(Some(Divergence {
            rewind_to: limit,
            anchor: None,
        }))
    }

    /// The cursor's verified anchor, if it has one.
    ///
    /// An empty hash means no verified block yet: a fresh chain, or one whose
    /// scanned range has produced no logs. It is not an anchor, and treating it as
    /// one would compare against bytes that are not a hash.
    async fn stored_anchor(&self, chain_id: i64) -> Result<Option<Checkpoint>, IngesterError> {
        let Some(cursor) = self.chain_state.fetch(chain_id).await? else {
            return Ok(None);
        };
        if cursor.last_block_hash.is_empty() {
            return Ok(None);
        }
        Ok(Some((cursor.last_block, cursor.last_block_hash)))
    }

    /// Heights to test, highest first: the anchor, then every stored hash down to
    /// `limit`. The first that still matches the chain bounds the fork.
    async fn checkpoints(
        &self,
        chain_id: i64,
        anchor: Checkpoint,
        limit: i64,
    ) -> Result<Vec<Checkpoint>, IngesterError> {
        let (anchor_block, _) = anchor;
        let mut out = vec![anchor];
        if anchor_block > limit {
            out.extend(
                self.raw_events
                    .block_hashes_desc(chain_id, limit, anchor_block - 1)
                    .await?,
            );
        }
        Ok(out)
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

        // Consumers stream `raw_events` by ascending id and re-read the
        // replacement rows on their own, but state derived from the deleted rows
        // is invisible to that cursor. The durable record lives in `chain_reorgs`,
        // written in the same transaction; this notify only reduces latency.
        if let Err(e) = self
            .raw_events
            .notify_reorg(chain_id, divergence.rewind_to)
            .await
        {
            warn!(chain_id, "reorg notify failed: {}", e);
        }
        Ok(deleted)
    }
}

/// Does the chain still report `stored` as the hash at `block`?
///
/// A block the node no longer has counts as not canonical, since it was almost
/// certainly orphaned.
async fn still_canonical(rpc: &DynRpc, block: i64, stored: &[u8]) -> Result<bool, IngesterError> {
    Ok(rpc
        .block_hash_at(block as u64)
        .await?
        .is_some_and(|h| h.0.as_slice() == stored))
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
    impl ChainStateRepo for FakeStore {
        async fn fetch(&self, _chain_id: i64) -> Result<Option<BlockCursor>, IngesterError> {
            Ok(self.cursor.lock().unwrap().clone())
        }
        async fn advance_scanned(&self, _c: i64, _s: i64) -> Result<(), IngesterError> {
            Ok(())
        }
    }

    #[async_trait]
    impl RawEventRepo for FakeStore {
        async fn block_hashes_desc(
            &self,
            _chain_id: i64,
            from_block: i64,
            to_block: i64,
        ) -> Result<Vec<Checkpoint>, IngesterError> {
            Ok(self
                .hashes
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| *n >= from_block && *n <= to_block)
                .cloned()
                .collect())
        }
        async fn notify_appended(&self, _c: i64) -> Result<(), IngesterError> {
            Ok(())
        }
        async fn notify_reorg(&self, _c: i64, _r: i64) -> Result<(), IngesterError> {
            Ok(())
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
        ReorgService::new(store.clone(), store.clone(), store.clone())
    }

    const DEPTH: u64 = 32;
    const FLOOR: i64 = 100;

    #[tokio::test]
    async fn an_untouched_chain_reports_no_divergence() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        let chain = FakeChain::new(100..=110, 0xa0) as DynRpc;
        let got = service(&store)
            .check_anchor(1, &chain, FLOOR, DEPTH)
            .await
            .unwrap();
        assert!(got.is_none(), "same hashes, no reorg");
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

        let divergence = service(&store)
            .check_anchor(1, &chain, FLOOR, DEPTH)
            .await
            .unwrap()
            .expect("fork detected");

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
        assert!(
            service(&store)
                .check_anchor(1, &chain, FLOOR, DEPTH)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_chain_with_no_cursor_is_not_a_divergence() {
        let store = FakeStore::with_cursor(None);
        let chain = FakeChain::new(100..=110, 0xa0) as DynRpc;
        assert!(
            service(&store)
                .check_anchor(1, &chain, FLOOR, DEPTH)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Deeper than `reorg_depth`: nothing in the window survives, so the whole
    /// window is discarded and no anchor remains to record.
    #[tokio::test]
    async fn a_fork_deeper_than_the_window_discards_the_whole_window() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        let chain = FakeChain::new(100..=110, 0xff) as DynRpc;

        let divergence = service(&store)
            .check_anchor(1, &chain, FLOOR, 4)
            .await
            .unwrap()
            .expect("fork detected");

        assert_eq!(divergence.rewind_to, 106, "anchor 110 minus depth 4");
        assert!(divergence.anchor.is_none(), "nothing survived to anchor on");
    }

    /// The walk must never propose discarding blocks below `start_block`, which
    /// the ingester never claimed to know.
    #[tokio::test]
    async fn the_walk_stops_at_the_configured_floor() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        let chain = FakeChain::new(100..=110, 0xff) as DynRpc;

        let divergence = service(&store)
            .check_anchor(1, &chain, FLOOR, 1_000)
            .await
            .unwrap()
            .expect("fork detected");

        assert_eq!(divergence.rewind_to, FLOOR);
    }

    /// A pruned or unavailable block is not a survivor. Treating a `None` hash as
    /// a match would anchor onto a block the node cannot produce.
    #[tokio::test]
    async fn a_block_the_node_no_longer_has_is_not_canonical() {
        let store = FakeStore::seeded(100..=110, 0xa0);
        // Only 100..=105 remain visible; the anchor at 110 is gone.
        let chain = FakeChain::new(100..=105, 0xa0) as DynRpc;

        let divergence = service(&store)
            .check_anchor(1, &chain, FLOOR, DEPTH)
            .await
            .unwrap()
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
