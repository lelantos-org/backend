//! snarkjs-shaped witness builder for `tree_update_batch.circom`.

use crate::domain::batch::MAX_L_BATCH;
use crate::services::tree::{AdvancedState, ReservedSlot};
use alloy::primitives::{FixedBytes, U256};
use common_crypto::tree::Field;
use groth16::TreeUpdateBatchWitness;

/// One escrowed deposit leaf. The circuit pins
/// `cv_dep == public_in · V^asset + rcv · H` for every slot flagged
/// `is_deposit`, so each leaf carries its own binding and there is no per-pair
/// aggregate.
#[derive(Debug, Clone)]
pub struct LeafDeposit {
    pub cv_dep: [U256; 2],
    pub leaf_asset: u64,
    pub leaf_public_in: u64,
    pub rcv: U256,
}

/// The circuit's leaf-indexed signals, every column padded to `MAX_L_BATCH`.
///
/// Held together rather than as five same-shaped vectors: both builders fill the
/// same slot index across all of them, and the circuit rejects a short or long
/// signal with no useful message. Zero is the padding the circuit and the
/// contract both enforce, so a column nobody fills is already correct.
struct LeafColumns {
    cms: Vec<String>,
    cv_dep: Vec<[String; 2]>,
    leaf_asset: Vec<String>,
    leaf_public_in: Vec<String>,
    is_deposit: Vec<String>,
    rcv: Vec<String>,
}

impl LeafColumns {
    fn zeroed() -> Self {
        let zeros = || vec!["0".to_string(); MAX_L_BATCH];
        Self {
            cms: zeros(),
            cv_dep: vec![["0".to_string(), "0".to_string()]; MAX_L_BATCH],
            leaf_asset: zeros(),
            leaf_public_in: zeros(),
            is_deposit: zeros(),
            rcv: zeros(),
        }
    }

    /// The commitment column, which both witnesses supply the same way.
    fn set_cms(&mut self, cms: &[FixedBytes<32>]) {
        for (i, cm) in cms.iter().enumerate() {
            self.cms[i] = bytes32_to_dec(&cm.0);
        }
    }
}

/// Spend-side witness: `TRANSACT_OUT` leaves, all with `is_deposit = 0`. The
/// transact SNARK already proves conservation, so the per-leaf deposit binding is
/// skipped and `rcv` stays zero.
pub fn build_spend(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    cms_real: &[FixedBytes<32>],
    cv_deps_real: &[[U256; 2]],
    z: String,
) -> TreeUpdateBatchWitness {
    debug_assert_eq!(cms_real.len(), cv_deps_real.len());

    let mut cols = LeafColumns::zeroed();
    cols.set_cms(cms_real);
    for (i, cv) in cv_deps_real.iter().enumerate() {
        cols.cv_dep[i] = [u256_to_dec(&cv[0]), u256_to_dec(&cv[1])];
    }

    build_inner(slot, advanced, cms_real.len() as u64, cols, z)
}

/// Flush-side witness, one leaf per escrowed deposit, so `deposits.len()` is the
/// leaf count. Padding slots emit zero for `cm`, `cv_dep`, `leaf_asset`,
/// `leaf_public_in`, `is_deposit` and `rcv`.
pub fn build_batch(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    real_cms: &[FixedBytes<32>],
    deposits: &[LeafDeposit],
    z: String,
) -> TreeUpdateBatchWitness {
    debug_assert_eq!(real_cms.len(), deposits.len());

    let mut cols = LeafColumns::zeroed();
    cols.set_cms(real_cms);
    for (i, d) in deposits.iter().enumerate() {
        cols.cv_dep[i] = [u256_to_dec(&d.cv_dep[0]), u256_to_dec(&d.cv_dep[1])];
        cols.leaf_asset[i] = d.leaf_asset.to_string();
        cols.leaf_public_in[i] = d.leaf_public_in.to_string();
        cols.is_deposit[i] = "1".to_string();
        cols.rcv[i] = u256_to_dec(&d.rcv);
    }

    build_inner(slot, advanced, deposits.len() as u64, cols, z)
}

fn build_inner(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    actual_count: u64,
    cols: LeafColumns,
    z: String,
) -> TreeUpdateBatchWitness {
    TreeUpdateBatchWitness {
        z,
        old_root: field_to_dec(&slot.old_root),
        new_root: field_to_dec(&advanced.new_root),
        start_index: slot.start_index.to_string(),
        actual_count: actual_count.to_string(),
        cms: cols.cms,
        cv_dep: cols.cv_dep,
        leaf_asset: cols.leaf_asset,
        leaf_public_in: cols.leaf_public_in,
        is_deposit: cols.is_deposit,
        rcv: cols.rcv,
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

fn field_to_dec(b: &Field) -> String {
    U256::from_be_bytes(*b).to_string()
}

fn bytes32_to_dec(b: &[u8; 32]) -> String {
    U256::from_be_bytes(*b).to_string()
}

fn u256_to_dec(v: &U256) -> String {
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::dto::TRANSACT_OUT;
    use crate::services::tree::{AdvancedState, ReservedSlot};

    /// A spend witness: `TRANSACT_OUT` leaves with no deposit binding, the shape
    /// both single-spend pipelines produce.
    fn spend_witness() -> TreeUpdateBatchWitness {
        let slot = ReservedSlot {
            start_index: 4,
            old_root: [1u8; 32],
            old_frontier: vec![[[2u8; 32], [3u8; 32], [4u8; 32]]; 10],
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
        build_spend(&slot, &advanced, &cms, &cv_deps, "12".to_string())
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
