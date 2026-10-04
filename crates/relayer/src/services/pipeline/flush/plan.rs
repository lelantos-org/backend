//! One tick's batch, and the bundle item that carries it.

use crate::adapters::abi::{IBundler, IMasp};
use crate::adapters::calldata::build_tu_batch_pub_inputs;
use crate::domain::batch::PaddedBatch;
use crate::domain::deposit::{EscrowLeaf, PendingDeposit};
use crate::domain::error::AppResult;
use crate::services::fees::gas_witness::{EntryPoint, GasWitness};
use crate::services::pipeline::batcher::{BundleItem, QueuedItem};
use crate::services::tree::{AdvancedState, ReservedSlot};
use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use crypto::tree::Field;
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
    /// Digest preimage the contract dropped from storage; a wrong field here
    /// reverts `DigestMismatch` for the whole batch.
    meta: Vec<IMasp::DepositMeta>,
    /// Every leaf at the circuit's full width.
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
            meta: pending
                .iter()
                .map(|p| IMasp::DepositMeta {
                    payer: Address::from(p.payer),
                    submittedAt: p.submitted_at,
                    fbps: p.fee_bps_at_submit,
                    pulled: p.pulled,
                })
                .collect(),
            batch: PaddedBatch::from_deposits(&leaves),
            fees_collected: pending.iter().map(|p| p.fee_in).sum(),
        }
    }

    /// `flushBatch` calldata for this batch against tree-update proof `tp`,
    /// under the batch's `digest` at `slot`.
    fn encode(
        &self,
        slot: &ReservedSlot,
        advanced: &AdvancedState,
        digest: U256,
        tp: IMasp::Proof,
    ) -> Vec<u8> {
        IMasp::flushBatchCall {
            ids: self.ids.iter().copied().map(U256::from).collect(),
            meta: self.meta.clone(),
            tp,
            tpi: build_tu_batch_pub_inputs(
                slot.start_index,
                &slot.old_root,
                &advanced.new_root,
                &self.batch,
                digest,
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

    fn batch(&self) -> &PaddedBatch {
        &self.plan.batch
    }

    fn merkle_root(&self) -> Option<Field> {
        None
    }

    fn encode(
        &self,
        slot: &ReservedSlot,
        advanced: &AdvancedState,
        digest: U256,
        tp: IMasp::Proof,
    ) -> AppResult<IBundler::Call> {
        Ok(IBundler::Call {
            target: self.pool,
            data: self.plan.encode(slot, advanced, digest, tp).into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Calldata and the witness carry each escrowed `inner`, and the circuit
    /// builds the leaf from it and the public amount, so the mirror inserts that
    /// commitment and not `inner`. A zero-value fee leaf is under asset 0.
    #[test]
    fn test_new_the_batch_carries_inner_and_its_leaves_are_built_from_it() {
        let paid = PendingDeposit::fixture();
        let unpaid = PendingDeposit {
            id: 2,
            fee_asset_id: 0,
            fee_in: 0,
            ..PendingDeposit::fixture()
        };
        let plan = BatchPlan::new(&[paid.clone(), unpaid.clone()]);

        let slots: Vec<[u8; 32]> = plan.batch.cms[..4].iter().map(|cm| cm.0).collect();
        assert_eq!(
            slots,
            [paid.inner, paid.fee_inner, unpaid.inner, unpaid.fee_inner]
        );
        let leaf = |asset, value, inner: &[u8; 32]| {
            crypto::note::commitment_from_inner(asset, value, inner).expect("canonical inner")
        };
        assert_eq!(
            plan.batch.leaves().expect("canonical deposits"),
            [
                leaf(paid.public_asset_id, paid.public_in, &paid.inner),
                leaf(paid.fee_asset_id, paid.fee_in, &paid.fee_inner),
                leaf(unpaid.public_asset_id, unpaid.public_in, &unpaid.inner),
                leaf(0, 0, &unpaid.fee_inner),
            ]
        );
    }

    /// `flushBatch` re-derives each escrow digest from the `DepositMeta` beside
    /// its id, and the digest binds the refund cap. So every entry must carry
    /// its own deposit's `pulled`, a plain escrow's 0 included: any other value
    /// reverts `DigestMismatch` for the whole batch, after the proof was paid
    /// for.
    #[test]
    fn test_encode_each_meta_entry_carries_its_own_deposits_refund_cap() {
        let yield_pull = U256::from(10_025_000_000_000_000u64);
        let plan = BatchPlan::new(&[
            PendingDeposit {
                id: 1,
                pulled: U256::ZERO,
                ..PendingDeposit::fixture()
            },
            PendingDeposit {
                id: 2,
                pulled: yield_pull,
                ..PendingDeposit::fixture()
            },
        ]);

        let slot = ReservedSlot {
            start_index: 0,
            old_root: [1u8; 32],
            old_frontier: Vec::new(),
            anchor_index: None,
        };
        let advanced = AdvancedState {
            new_root: [2u8; 32],
        };
        let tp = IMasp::Proof {
            a: [U256::ZERO; 2],
            b: [[U256::ZERO; 2]; 2],
            c: [U256::ZERO; 2],
        };
        let data = plan.encode(&slot, &advanced, U256::ZERO, tp);

        let call = IMasp::flushBatchCall::abi_decode(&data, true).expect("flushBatch calldata");
        assert_eq!(call.ids, [U256::from(1), U256::from(2)]);
        let caps: Vec<U256> = call.meta.iter().map(|m| m.pulled).collect();
        assert_eq!(caps, [U256::ZERO, yield_pull]);
    }
}
