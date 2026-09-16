//! One tick's batch, and the bundle item that carries it.

use crate::adapters::abi::{IBundler, IMasp};
use crate::adapters::calldata::build_tu_batch_pub_inputs;
use crate::domain::batch::PaddedBatch;
use crate::domain::deposit::{EscrowLeaf, PendingDeposit};
use crate::domain::error::AppResult;
use crate::domain::fiat_shamir;
use crate::services::fees::gas_witness::{EntryPoint, GasWitness};
use crate::services::pipeline::batcher::{BundleItem, QueuedItem};
use crate::services::tree::{AdvancedState, ReservedSlot};
use crate::services::witness;
use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use crypto::tree::Field;
use groth16::TreeUpdateBatchWitness;
use std::sync::Arc;

/// One tick's batch, in every shape the flush path needs it.
///
/// Built once from the pending deposits so that the leaf-indexed arrays cannot
/// disagree: they all derive from the single `leaves` vector, which is
/// [`PendingDeposit::leaves`] flattened in the order `_drainDeposit` reads the
/// pair back at `2i` and `2i + 1`.
pub(super) struct BatchPlan {
    /// Deposits in the batch. `flushBatch` inserts `2n` leaves and the contract
    /// enforces the doubling, so an odd leaf count is a `BadBatchSize`.
    pub(super) n: usize,
    /// Deposit ids, in batch order.
    pub(super) ids: Vec<u64>,
    /// `(cm, cv_dep)` pairs as `TreeMirror` wants them.
    tree_leaves: Vec<(Field, [U256; 2])>,
    /// Digest preimage the contract dropped from storage; a wrong field here
    /// reverts `DigestMismatch` for the whole batch.
    meta: Vec<IMasp::DepositMeta>,
    /// Every leaf at the circuit's full width, blinders included.
    batch: PaddedBatch,
    /// Escrowed fee, summed over the batch. Deposits in one batch can pay their
    /// fee notes in different assets, so this is a batch total rather than an
    /// amount of a single token, and is meaningful only alongside the per-deposit
    /// lines `fee_gate` emits.
    pub(super) fees_collected: u64,
}

impl BatchPlan {
    pub(super) fn new(pending: &[PendingDeposit]) -> Self {
        let leaves: Vec<EscrowLeaf> = pending.iter().flat_map(PendingDeposit::leaves).collect();
        Self {
            n: pending.len(),
            ids: pending.iter().map(|p| p.id).collect(),
            tree_leaves: leaves.iter().map(|l| (l.cm, l.cv_dep)).collect(),
            meta: pending
                .iter()
                .map(|p| IMasp::DepositMeta {
                    payer: Address::from(p.payer),
                    submittedAt: p.submitted_at,
                    fbps: p.fee_bps_at_submit,
                })
                .collect(),
            batch: PaddedBatch::from_deposits(&leaves),
            fees_collected: pending.iter().map(|p| p.fee_in).sum(),
        }
    }

    /// `flushBatch` calldata for this batch against tree-update proof `tp`.
    fn encode(&self, slot: &ReservedSlot, advanced: &AdvancedState, tp: IMasp::Proof) -> Vec<u8> {
        IMasp::flushBatchCall {
            ids: self.ids.iter().copied().map(U256::from).collect(),
            meta: self.meta.clone(),
            tp,
            tpi: build_tu_batch_pub_inputs(
                slot.start_index,
                &slot.old_root,
                &advanced.new_root,
                &self.batch,
            ),
        }
        .abi_encode()
    }
}

/// One tick's `flushBatch`, as the batcher bundles it.
pub(super) struct FlushItem {
    pub(super) plan: Arc<BatchPlan>,
    pub(super) pool: Address,
}

impl BundleItem for FlushItem {
    fn entry(&self) -> EntryPoint {
        EntryPoint::Flush
    }

    fn leaves(&self) -> Vec<(Field, [U256; 2])> {
        self.plan.tree_leaves.clone()
    }

    fn merkle_root(&self) -> Option<Field> {
        None
    }

    fn witness(&self, slot: &ReservedSlot, advanced: &AdvancedState) -> TreeUpdateBatchWitness {
        let z = fiat_shamir::compute_z(
            &slot.old_root,
            &advanced.new_root,
            slot.start_index,
            &self.plan.batch,
        );
        witness::build(slot, advanced, &self.plan.batch, z)
    }

    fn encode(
        &self,
        slot: &ReservedSlot,
        advanced: &AdvancedState,
        tp: IMasp::Proof,
    ) -> AppResult<IBundler::Call> {
        Ok(IBundler::Call {
            target: self.pool,
            data: self.plan.encode(slot, advanced, tp).into(),
        })
    }

    /// `EntryPoint::Flush` is quoted per deposit, so the batch weighs `n` of it.
    fn gas_weight(&self, gas: &GasWitness) -> u64 {
        gas.gas_for(EntryPoint::Flush)
            .saturating_mul(self.plan.n as u64)
    }

    fn view(&self) -> QueuedItem {
        QueuedItem {
            kind: EntryPoint::Flush.as_str(),
            nullifiers: Vec::new(),
            deposit_ids: self.plan.ids.clone(),
        }
    }
}
