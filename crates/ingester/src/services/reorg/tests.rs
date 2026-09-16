//! Anchor checks and rewinds against a synthetic chain and store.

use super::*;
use crate::adapters::rpc::ChainRpc;
use crate::domain::models::BlockMeta;
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
    async fn fetch_logs(&self, _a: Address, _f: u64, _t: u64) -> Result<Vec<Log>, IngesterError> {
        Ok(Vec::new())
    }
    async fn fetch_block_meta(&self, _b: &[u64]) -> Result<HashMap<u64, BlockMeta>, IngesterError> {
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
            .map(|n| Checkpoint {
                block: n,
                hash: B256::repeat_byte(tag ^ (n as u8)).0.to_vec(),
            })
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
            .filter(|c| c.block >= from_block && c.block <= to_block)
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
    let chain_hash = chain.block_hash_at(anchor.block as u64).await.unwrap();
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
    assert_eq!(divergence.anchor.expect("survivor").block, 107);
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
        anchor: Some(Checkpoint {
            block: 107,
            hash: hash(0x07),
        }),
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
