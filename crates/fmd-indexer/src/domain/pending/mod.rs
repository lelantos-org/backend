//! Assemble a batch of raw events into the rows one consume tick may commit.
//!
//! Pure and synchronous: everything the plan needs is passed in, so the decision
//! is testable without a database.
//!
//! - `leaf`: one leaf's FMD payload, and what the tree and `notes` take from it.
//! - `tx`: the per-transaction accumulator linking roots to their leaves.
//! - `batch`: a window grouped by transaction, drained up to the first one not
//!   fully observed.

mod batch;
mod leaf;
mod tx;

pub use leaf::{LeafPayload, TreeLeaf};

use crate::domain::error::FmdIndexerError;
use crate::domain::escrow::EscrowedMap;
use batch::Batch;
use database::models::{NewNote, RawEventRow};

/// One spend the plan records, before the repository assigns its per-chain
/// ordinal. Plain data: the numbered, `Insertable` form is repository-private.
#[derive(Debug, Clone)]
pub struct NewSpentNullifier {
    pub chain_id: i64,
    pub block_number: i64,
    pub log_index: i32,
    pub nf: Vec<u8>,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}

/// Debug-printable: these are public chain values, and a plan is the first thing
/// to dump when a tick commits something unexpected.
#[derive(Debug)]
pub struct CommitPlan {
    pub notes: Vec<NewNote>,
    /// Contiguous from `leaves[0].leaf_index`, and a superset of `notes`: it
    /// carries the leaves dropped as holes too, so it advances the tree exactly
    /// as the contract did.
    pub leaves: Vec<TreeLeaf>,
    pub spent_nfs: Vec<NewSpentNullifier>,
    pub last_event_id: i64,
    pub last_block_number: i64,
}

/// Group raw events by `tx_hash`, decode them, and produce a commit plan up to
/// the first transaction that is not fully observed. `None` when nothing is
/// ready.
///
/// `escrowed` holds pre-resolved `DepositEscrowed` payloads keyed by deposit id.
/// A `DepositFlushed` referencing a missing one defers its whole transaction
/// until the escrow event has been ingested.
pub fn plan_commit(
    rows: &[RawEventRow],
    chain_id: i64,
    after: i64,
    escrowed: &EscrowedMap,
) -> Result<Option<CommitPlan>, FmdIndexerError> {
    Batch::assemble(rows, chain_id, escrowed).map(|batch| batch.commit_through(chain_id, after))
}

#[cfg(test)]
mod tests;
