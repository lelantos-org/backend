//! Fold a tick's leaves into the chain's stored commitment-tree frontier.
//!
//! Split out of the tick itself because it is the one part of a commit that is
//! positional rather than idempotent: `notes` absorbs a replay through `ON
//! CONFLICT DO NOTHING`, while folding the same leaf twice moves the root
//! somewhere the chain never was. Everything here exists to keep that from
//! happening.

use crate::domain::error::{FmdIndexerError, Result};
use crate::domain::pending::TreeLeaf;
use crate::repositories::notes::NotesRepo;
use crate::repositories::tree_state::{TreeStateRepo, TreeStateRow};
use crypto::tree::{DEPTH, Field, Frontier, encode_frontier};
use database::models::LeafInputsRow;
use tracing::{info, warn};

/// Leaves read per round trip during [`backfill`].
const LEAF_PAGE: i64 = 100_000;

fn tree_err(e: crypto::tree::TreeError) -> FmdIndexerError {
    FmdIndexerError::Decode(e.to_string())
}

/// `notes` is missing a leaf the tree counted, so no frontier can be folded from
/// it. `found` is `-1` when the range simply ended early.
fn hole(chain_id: i64, expected: i64, found: i64) -> FmdIndexerError {
    FmdIndexerError::Decode(format!(
        "chain {chain_id} has no note at leaf_index {expected} (found {found}); \
         cannot backfill the tree from notes"
    ))
}

fn leaf_hash_of(row: &LeafInputsRow) -> Result<Field> {
    let cm = crypto::tree::field_from_bytes(&row.cm).map_err(tree_err)?;
    crypto::tree::leaf_hash(
        &cm,
        &crate::domain::convert::bigdec_to_field(&row.cv_dep_x),
        &crate::domain::convert::bigdec_to_field(&row.cv_dep_y),
    )
    .map_err(tree_err)
}

/// Fold this tick's leaves into the chain's stored frontier.
///
/// Reloaded from the row each time rather than cached in the service. The
/// frontier is a kilobyte and `resume` costs `DEPTH` hashes, so re-reading is
/// cheap next to the tick that produced the leaves, and it means the stored
/// row is the only state: a restart, a reorg rewind, or a second writer
/// taking the chain lock all resolve on the next tick with no in-memory copy
/// to invalidate.
pub(super) async fn advance(
    notes: &dyn NotesRepo,
    tree_state: &dyn TreeStateRepo,
    chain_id: i64,
    leaves: &[TreeLeaf],
) -> Result<()> {
    let Some(first) = leaves.first() else {
        return Ok(());
    };

    let stored = tree_state.load(chain_id).await?;
    let mut from = stored.as_ref().map_or(0, |row| row.leaf_count);
    let mut frontier = match &stored {
        Some(row) => Frontier::resume(
            DEPTH,
            row.leaf_count as u64,
            crypto::tree::decode_frontier(DEPTH, &row.frontier)
                .map_err(|e| FmdIndexerError::Decode(format!("stored frontier: {e}")))?,
            crypto::tree::field_from_bytes(&row.root)
                .map_err(|e| FmdIndexerError::Decode(format!("stored root: {e}")))?,
        ),
        None => Frontier::new(DEPTH),
    }
    .map_err(|e| FmdIndexerError::Decode(e.to_string()))?;

    let start = first.leaf_index;
    if start > from {
        if stored.is_some() {
            // The tree is positional, so folding across a gap puts every later
            // leaf one place out and the root stops matching the chain for
            // good. Nothing here can repair it, so the tick fails and the
            // chain stalls visibly rather than publishing a root no wallet can
            // verify.
            return Err(FmdIndexerError::Decode(format!(
                "tree gap on chain {chain_id}: stored {from} leaves, plan starts at {start}"
            )));
        }
        // No row and leaves already behind us: this chain was indexed before
        // the table existed. The consume cursor is long past those events and
        // will never replay them, so the history has to be folded in once from
        // `notes` before the tick's leaves can be appended.
        frontier = backfill(notes, tree_state, chain_id, start).await?;
        // The backfill covers leaves `0..start`, so that is where this tick
        // appends from. Leaving `from` at 0 would make the skip below
        // underflow and silently drop the whole tick.
        from = start;
    }

    // A replay -- of the batch, or of the whole chain after a reorg rewind --
    // re-presents leaves already folded in. `notes` absorbs that with ON
    // CONFLICT DO NOTHING; a frontier has to skip the prefix by hand, since
    // folding a leaf twice moves the root somewhere the chain never was.
    let already = (from - start) as usize;
    let Some(fresh) = leaves.get(already..).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    frontier
        .extend(fresh.iter().map(|leaf| leaf.hash))
        .map_err(|e| FmdIndexerError::Decode(e.to_string()))?;

    let next = TreeStateRow {
        chain_id,
        leaf_count: frontier.leaf_count() as i64,
        root: frontier.root().to_vec(),
        frontier: encode_frontier(&frontier.slots()),
    };
    if !tree_state.advance(from, &next).await? {
        // Another writer moved the row between the load and the write. The
        // next tick reloads and re-derives, so this costs a tick rather than
        // correctness.
        warn!(
            chain_id,
            from,
            to = next.leaf_count,
            "tree state advance rejected; base moved under this tick"
        );
        return Ok(());
    }
    metrics::gauge!(
        shared::metrics::name::TREE_STATE_LEAVES,
        "chain_id" => chain_id.to_string(),
    )
    .set(next.leaf_count as f64);
    Ok(())
}

/// Fold `notes` into a frontier covering leaves `0..leaves`, for a chain that
/// was indexed before `tree_state` existed.
///
/// Folds each page with [`Frontier::extend`] rather than a `push` loop: the
/// batch carries only completed groups up and folds the root once per page,
/// which is O(N) over the chain's whole history against `DEPTH` hashes per
/// leaf, and it holds a page rather than the 1.33 nodes per leaf a
/// `MerkleTree` would materialise for the same answer.
async fn backfill(
    notes: &dyn NotesRepo,
    tree_state: &dyn TreeStateRepo,
    chain_id: i64,
    leaves: i64,
) -> Result<Frontier> {
    let started = std::time::Instant::now();
    warn!(
        chain_id,
        leaves, "no stored tree state; folding note history into a frontier"
    );

    let mut frontier = Frontier::new(DEPTH).map_err(tree_err)?;
    let mut next = 0i64;
    while next < leaves {
        let rows = notes
            .leaf_inputs(chain_id, next, (next + LEAF_PAGE).min(leaves))
            .await?;
        let mut hashes = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let expected = next + i as i64;
            if row.leaf_index != expected {
                return Err(hole(chain_id, expected, row.leaf_index));
            }
            hashes.push(leaf_hash_of(row)?);
        }
        if rows.len() as i64 != (next + LEAF_PAGE).min(leaves) - next {
            // Short page: `notes` stops before the tree does, so the tail of
            // the range is missing rather than out of order.
            return Err(hole(chain_id, next + rows.len() as i64, -1));
        }
        next += rows.len() as i64;
        frontier.extend(hashes).map_err(tree_err)?;
    }

    let root = frontier.root();
    // The one external check there is. A frontier folded from `notes` is
    // self-consistent whether or not it is right; only the chain's own
    // published root can say which. Refusing here leaves the row absent, so
    // `/v1/tree-state` reports an empty tree -- visibly wrong, where a stored
    // bad frontier would be silently wrong forever after.
    match tree_state.published_root(chain_id, leaves).await? {
        Some(published) if published == root => {}
        Some(published) => {
            return Err(FmdIndexerError::Decode(format!(
                "backfilled root for chain {chain_id} at {leaves} leaves is {}, \
                 but the chain published {}",
                hex::encode(root),
                hex::encode(published),
            )));
        }
        None => {
            return Err(FmdIndexerError::Decode(format!(
                "no tree_advances row for chain {chain_id} ending at {leaves} leaves; \
                 cannot verify the backfilled root"
            )));
        }
    }

    info!(
        chain_id,
        leaves,
        elapsed_s = started.elapsed().as_secs_f64(),
        "tree backfill verified against the chain's published root"
    );
    Ok(frontier)
}
