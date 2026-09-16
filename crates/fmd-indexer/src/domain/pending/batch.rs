//! A window of rows grouped by transaction, and drained into a plan.

use super::CommitPlan;
use super::tx::{LeafOrder, PendingTx, RowCtx, TxState};
use crate::domain::error::FmdIndexerError;
use crate::domain::escrow::EscrowedMap;
use chain_types::decode;
use database::models::RawEventRow;
use shared::entities::EventKind;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use tracing::{error, warn};

/// Decoded rows grouped by transaction, in first-seen and therefore id order.
pub(super) struct Batch {
    pub(super) by_tx: HashMap<Vec<u8>, PendingTx>,
    pub(super) order: Vec<Vec<u8>>,
}

impl Batch {
    pub(super) fn assemble(
        rows: &[RawEventRow],
        chain_id: i64,
        escrowed: &EscrowedMap,
    ) -> Result<Self, FmdIndexerError> {
        let mut batch = Self {
            by_tx: HashMap::new(),
            order: Vec::new(),
        };

        for row in rows {
            let tx = batch.tx_for(row);
            tx.observe(row);

            let Some(kind) = EventKind::from_i16(row.event_kind) else {
                // Not a leaf kind by construction: every leaf kind is known, so
                // this row cannot be one the transaction is waiting on.
                warn!(
                    chain_id,
                    event_kind = row.event_kind,
                    block_number = row.block_number,
                    log_index = row.log_index,
                    "unknown event kind; skipping"
                );
                continue;
            };

            let cx = RowCtx {
                chain_id,
                row,
                escrowed,
            };
            let decoded = match decode::decode(kind, &row.topics, &row.data) {
                Ok(decoded) => decoded,
                Err(e) => {
                    // A leaf event that will not decode never will, but the contract
                    // inserted its leaves regardless, so claim them as holes and let
                    // the transaction complete. A `DepositFlushed` stands for two:
                    // the deposit's note and the fee note it emits no event for.
                    let (order, leaves) = match kind {
                        EventKind::NoteCreated => (LeafOrder::RootLeads, 1),
                        EventKind::DepositFlushed => (LeafOrder::LeafLeads, 2),
                        _ => {
                            warn!(chain_id, block_number = row.block_number, log_index = row.log_index, error = %e, "decode failed; skipping");
                            continue;
                        }
                    };
                    for _ in 0..leaves {
                        tx.claim_hole(&cx, order, "decode failed");
                    }
                    continue;
                }
            };

            for event in decoded {
                tx.apply(event, &cx)?;
            }
        }
        Ok(batch)
    }

    pub(super) fn tx_for(&mut self, row: &RawEventRow) -> &mut PendingTx {
        match self.by_tx.entry(row.tx_hash.clone()) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => {
                self.order.push(v.key().clone());
                v.insert(PendingTx::new(row))
            }
        }
    }

    /// Drain transactions in order until one is not [`TxState::Ready`].
    pub(super) fn commit_through(mut self, chain_id: i64, after: i64) -> Option<CommitPlan> {
        let mut plan = CommitPlan {
            notes: Vec::new(),
            leaves: Vec::new(),
            spent_nfs: Vec::new(),
            last_event_id: after,
            last_block_number: 0,
        };

        for tx_hash in &self.order {
            let tx = self.by_tx.get_mut(tx_hash).expect("assembled from `order`");
            if tx.state() == TxState::Pending {
                break;
            }
            if tx.skipped > 0 {
                error!(
                    chain_id,
                    tx_hash = %hex::encode(tx_hash),
                    block_number = tx.last_block,
                    skipped = tx.skipped,
                    "committing tx with unusable leaves; leaf_index range has holes"
                );
            }
            plan.last_event_id = tx.last_id;
            plan.last_block_number = tx.last_block;
            plan.notes.append(&mut tx.notes);
            plan.leaves.append(&mut tx.leaves);
            plan.spent_nfs.append(&mut tx.spent_nfs);
        }

        (plan.last_event_id != after).then_some(plan)
    }
}
