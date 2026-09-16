//! snarkjs-shaped witness builder for `tree_update_batch.circom`.

use crate::domain::batch::PaddedBatch;
use crate::services::tree::{AdvancedState, ReservedSlot};
use alloy::primitives::U256;
use crypto::tree::Field;
use groth16::TreeUpdateBatchWitness;

/// The `tree_update_batch` witness for `batch` advancing the tree from `slot` to
/// `advanced`, under challenge `z`.
///
/// Every leaf-indexed signal is read from `batch`, already padded to
/// `MAX_L_BATCH`: the circuit rejects a short or long signal with no useful
/// message, and zero is the padding the circuit and the contract both enforce.
/// A spend batch carries no deposit binding, so its `is_deposit`, `leaf_asset`,
/// `leaf_public_in` and `rcv` columns are already zero.
pub fn build(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    batch: &PaddedBatch,
    z: String,
) -> TreeUpdateBatchWitness {
    TreeUpdateBatchWitness {
        z,
        old_root: field_to_dec(&slot.old_root),
        new_root: field_to_dec(&advanced.new_root),
        start_index: slot.start_index.to_string(),
        actual_count: batch.actual_count.to_string(),
        cms: batch.cms.iter().map(|cm| field_to_dec(&cm.0)).collect(),
        cv_dep: batch
            .cv_deps
            .iter()
            .map(|cv| [cv[0].to_string(), cv[1].to_string()])
            .collect(),
        leaf_asset: dec_column(&batch.leaf_asset),
        leaf_public_in: dec_column(&batch.leaf_public_in),
        is_deposit: dec_column(&batch.is_deposit),
        rcv: dec_column(&batch.rcv),
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

    /// A spend witness: `TRANSACT_OUT` leaves with no deposit binding, the shape
    /// both single-spend pipelines produce.
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
        let cv_deps: Vec<[U256; 2]> = (0..TRANSACT_OUT)
            .map(|i| [U256::from(8u8 + i as u8), U256::from(9u8 + i as u8)])
            .collect();
        build(
            &slot,
            &advanced,
            &PaddedBatch::from_spend(&cms, &cv_deps),
            "12".to_string(),
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
        assert_eq!(width("cv_dep"), 2 * MAX_L_BATCH, "flattened BJJ points");
        assert_eq!(width("leaf_asset"), MAX_L_BATCH);
        assert_eq!(width("leaf_public_in"), MAX_L_BATCH);
        assert_eq!(width("is_deposit"), MAX_L_BATCH);
        assert_eq!(width("rcv"), MAX_L_BATCH);
        assert_eq!(width("frontier_in"), 3 * 10, "depth rows of 3 siblings");
    }

    /// Padding slots must be zero: the circuit and the contract both enforce it,
    /// and a stray value is otherwise invisible until the prove.
    #[test]
    fn padding_slots_are_zero() {
        let w = spend_witness();
        let signals = w.signals();
        for (name, from) in [("cms", TRANSACT_OUT), ("cv_dep", 2 * TRANSACT_OUT)] {
            for (i, v) in signal(&signals, name).iter().enumerate().skip(from) {
                assert_eq!(*v, "0", "{name}[{i}] should be padding");
            }
        }
        for name in ["leaf_asset", "leaf_public_in", "is_deposit", "rcv"] {
            assert!(signal(&signals, name).iter().all(|v| *v == "0"), "{name}");
        }
    }
}
