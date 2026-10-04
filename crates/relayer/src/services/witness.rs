//! What a reserved batch proves: its challenge, and the snarkjs-shaped witness
//! for `tree_update_batch.circom`.

use crate::domain::batch::PaddedBatch;
use crate::domain::error::AppResult;
use crate::domain::fiat_shamir::{self, BatchChallenge};
use crate::services::tree::{AdvancedState, ReservedSlot};
use alloy::primitives::U256;
use crypto::tree::Field;
use groth16::TreeUpdateBatchWitness;

/// The digest and the challenge of `batch` advancing the tree from `slot` to
/// `advanced`: `digest` goes in calldata and `z` into [`build`].
pub fn challenge(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    batch: &PaddedBatch,
) -> AppResult<BatchChallenge> {
    fiat_shamir::compress(&slot.old_root, &advanced.new_root, slot.start_index, batch)
}

/// The `tree_update_batch` witness for `batch` advancing the tree from `slot` to
/// `advanced`, under challenge `z`.
///
/// Every leaf-indexed signal is read from `batch`, already padded to
/// `MAX_L_BATCH`: the circuit rejects a short or long signal with no useful
/// message, and zero is the padding the circuit and the contract both enforce.
/// A spend batch has no deposit leaves, so its `is_deposit`, `leaf_asset` and
/// `leaf_public_in` columns are already zero.
pub fn build(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    batch: &PaddedBatch,
    z: U256,
) -> TreeUpdateBatchWitness {
    TreeUpdateBatchWitness {
        z: z.to_string(),
        old_root: field_to_dec(&slot.old_root),
        new_root: field_to_dec(&advanced.new_root),
        start_index: slot.start_index.to_string(),
        actual_count: batch.actual_count.to_string(),
        cms: batch.cms.iter().map(|cm| field_to_dec(&cm.0)).collect(),
        leaf_asset: dec_column(&batch.leaf_asset),
        leaf_public_in: dec_column(&batch.leaf_public_in),
        is_deposit: dec_column(&batch.is_deposit),
        frontier_in: slot
            .old_frontier
            .iter()
            .map(|row| {
                [
                    field_to_dec(&row[0]),
                    field_to_dec(&row[1]),
                    field_to_dec(&row[2]),
                ]
            })
            .collect(),
    }
}

fn dec_column<T: ToString>(column: &[T]) -> Vec<String> {
    column.iter().map(ToString::to_string).collect()
}

fn field_to_dec(b: &Field) -> String {
    U256::from_be_bytes(*b).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::batch::MAX_L_BATCH;
    use crate::domain::dto::TRANSACT_OUT;
    use alloy::primitives::FixedBytes;

    /// A spend witness: `TRANSACT_OUT` leaves and no deposit leaf, the shape both
    /// single-spend pipelines produce.
    fn spend_witness() -> TreeUpdateBatchWitness {
        let slot = ReservedSlot {
            start_index: 4,
            old_root: [1u8; 32],
            old_frontier: vec![[[2u8; 32], [3u8; 32], [4u8; 32]]; 10],
            anchor_index: None,
        };
        let advanced = AdvancedState {
            new_root: [5u8; 32],
        };
        let cms: Vec<FixedBytes<32>> = (0..TRANSACT_OUT)
            .map(|i| FixedBytes::<32>::from([6u8 + i as u8; 32]))
            .collect();
        build(
            &slot,
            &advanced,
            &PaddedBatch::from_spend(&cms),
            U256::from(12u8),
        )
    }

    /// One signal's flattened values, by the name the circuit declares.
    fn signal<'a>(signals: &'a [(&str, Vec<&str>)], name: &str) -> &'a [&'a str] {
        signals
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .1
            .as_slice()
    }

    /// The circuit declares fixed-width arrays, and a short or long signal is
    /// rejected inside circom with no useful message, so the widths are pinned
    /// here. `TreeUpdateBatchWitness::signals` does the flattening; what this
    /// builder owes it is the padding out to `MAX_L_BATCH`.
    #[test]
    fn signal_widths_match_the_circuit_declaration() {
        let w = spend_witness();
        let signals = w.signals();
        let width = |name: &str| signal(&signals, name).len();

        for scalar in ["z", "old_root", "new_root", "start_index", "actual_count"] {
            assert_eq!(width(scalar), 1, "{scalar}");
        }
        assert_eq!(width("cms"), MAX_L_BATCH);
        assert_eq!(width("leaf_asset"), MAX_L_BATCH);
        assert_eq!(width("leaf_public_in"), MAX_L_BATCH);
        assert_eq!(width("is_deposit"), MAX_L_BATCH);
        assert_eq!(width("frontier_in"), 3 * 10, "depth rows of 3 siblings");
    }

    /// Padding slots must be zero: the circuit and the contract both enforce it,
    /// and a stray value is otherwise invisible until the prove.
    #[test]
    fn padding_slots_are_zero() {
        let w = spend_witness();
        let signals = w.signals();
        for (i, v) in signal(&signals, "cms")
            .iter()
            .enumerate()
            .skip(TRANSACT_OUT)
        {
            assert_eq!(*v, "0", "cms[{i}] should be padding");
        }
        for name in ["leaf_asset", "leaf_public_in", "is_deposit"] {
            assert!(signal(&signals, name).iter().all(|v| *v == "0"), "{name}");
        }
    }
}
