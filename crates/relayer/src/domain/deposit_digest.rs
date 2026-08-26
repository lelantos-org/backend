//! Off-chain twin of `MASP._depositDigest`.
//!
//! The contract stores a deposit as a single keccak digest and drops every field
//! that went into it. `flushBatch` re-derives the digest from the `DepositMeta`
//! the relayer replays and reverts `DigestMismatch(id)` for the whole batch if it
//! differs. Deriving it here lets the flush pipeline drop a deposit whose
//! replayed fields cannot match before paying for a `tree_update_batch` Groth16.
//!
//! Must stay byte-identical to `contracts/src/MASP.sol::_depositDigest`.

use crate::services::deposit_mempool::PendingDeposit;
use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::sol_types::SolValue;

/// The contract narrows `publicIn` to `uint48` after bounding it, so a wider
/// value both hashes differently and reverts `PublicInTooLarge` first.
pub const MAX_PUBLIC_IN: u64 = (1u64 << 48) - 1;

/// `keccak256(abi.encode(address(this), block.chainid, id, cm, cvDep,
/// publicAssetId, publicIn, feeBpsAtSubmit, payer, submittedAt, feeIn, feeCm,
/// feeCvDep))`.
///
/// `abi.encode` is not packed, so every static field occupies a full word and the
/// declared Solidity widths (`uint64`, `uint48`, `uint16`, `uint32`) encode
/// identically to `U256` provided the value fits, which [`MAX_PUBLIC_IN`] and the
/// `PendingDeposit` field types guarantee. `cvDep` and `feeCvDep` are static
/// `uint256[2]`, so each lands inline as two words.
///
/// The trailing three fields bind the relayer's fee note, and are preimage for
/// the same reason the depositor's leaf is: `flushBatch` supplies both leaves
/// from calldata, so a flusher able to vary them could mint itself an arbitrary
/// note.
pub fn deposit_digest(masp: Address, chain_id: u64, d: &PendingDeposit) -> B256 {
    let preimage = (
        masp,
        U256::from(chain_id),
        U256::from(d.id),
        B256::from(d.cm),
        d.cv_dep,
        U256::from(d.public_asset_id),
        U256::from(d.public_in),
        U256::from(d.fee_bps_at_submit),
        Address::from(d.payer),
        U256::from(d.submitted_at),
        U256::from(d.fee_in),
        B256::from(d.fee_cm),
        d.fee_cv_dep,
    );
    keccak256(preimage.abi_encode_params())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as JsonValue;

    /// Every field distinct and non-zero, so a swapped pair or a dropped
    /// field cannot coincidentally hash to the same digest.
    fn deposit() -> PendingDeposit {
        PendingDeposit {
            id: 1,
            cm: [0xab; 32],
            public_asset_id: 2,
            public_in: 3,
            fee_bps_at_submit: 4,
            payer: [0xcd; 20],
            submitted_at: 5,
            cv_dep: [U256::from(6), U256::from(7)],
            rcv: U256::from(8),
            fee_in: 9,
            fee_cm: [0xef; 32],
            fee_cv_dep: [U256::from(10), U256::from(11)],
            fee_rcv: U256::from(12),
            fee_aux: JsonValue::Null,
        }
    }

    fn b256(hex: &str) -> B256 {
        hex.parse().expect("golden digest is valid hex")
    }

    /// Golden vector from `cast`, which encodes exactly as Solidity's
    /// `abi.encode` does:
    ///
    /// ```sh
    /// cast keccak "$(cast abi-encode \
    ///   'f(address,uint256,uint256,bytes32,uint256[2],uint64,uint48,uint16,address,uint32,\
    ///      uint48,bytes32,uint256[2])' \
    ///   0x1111111111111111111111111111111111111111 31337 42 \
    ///   0x2222222222222222222222222222222222222222222222222222222222222222 \
    ///   '[3,4]' 7 1000000 25 0x3333333333333333333333333333333333333333 123456 \
    ///   9 0x4444444444444444444444444444444444444444444444444444444444444444 '[5,6]')"
    /// ```
    ///
    /// Drift here quarantines every deposit on every chain, so this and the test
    /// below pin against external encoders rather than against `deposit_digest`.
    #[test]
    fn the_digest_matches_the_contract_encoding() {
        let d = PendingDeposit {
            id: 42,
            cm: [0x22; 32],
            public_asset_id: 7,
            public_in: 1_000_000,
            fee_bps_at_submit: 25,
            payer: [0x33; 20],
            submitted_at: 123_456,
            cv_dep: [U256::from(3), U256::from(4)],
            rcv: U256::ZERO,
            fee_in: 9,
            fee_cm: [0x44; 32],
            fee_cv_dep: [U256::from(5), U256::from(6)],
            fee_rcv: U256::ZERO,
            fee_aux: JsonValue::Null,
        };
        assert_eq!(
            deposit_digest(Address::from([0x11; 20]), 31337, &d),
            b256("0x2ac6af6b953f74ca86136d4b238f0e897949047cd8a2e63da1b8a8dfb4a74ecf")
        );
    }

    /// The same check against a real deployment rather than an encoder: a
    /// `MASP.deposit()` in the Foundry harness
    /// (`contracts/test/MASP.escrowEventRoundtrip.t.sol`), with the preimage taken
    /// from the `DepositEscrowed` log as the indexer takes it and `submittedAt`
    /// from the emitting block. Catches a field the relayer sources from the wrong
    /// column, which the encoder test cannot.
    #[test]
    fn the_digest_matches_a_deposit_a_deployed_masp_escrowed() {
        let d = PendingDeposit {
            id: 0,
            cm: U256::from(0x111).to_be_bytes(),
            public_asset_id: 1,
            public_in: 100,
            fee_bps_at_submit: 25,
            payer: address("0x000000000000000000000000000000000000Face").into(),
            submitted_at: 1,
            cv_dep: [U256::from(0xaa1), U256::from(0xaa2)],
            rcv: U256::from(0xccc),
            // The harness deposits with a zero-value fee note, a valid shape: a
            // subsidised chain sets `feeIn` to zero and the leaf is still minted
            // and spendable.
            fee_in: 0,
            fee_cm: U256::from(0xfee).to_be_bytes(),
            fee_cv_dep: [U256::ZERO, U256::ZERO],
            fee_rcv: U256::ZERO,
            fee_aux: JsonValue::Null,
        };
        assert_eq!(
            deposit_digest(
                address("0xc7183455a4C133Ae270771860664b6B7ec320bB1"),
                31337,
                &d
            ),
            b256("0xf5e086b8cb99abac3e7d4f3127326701062baca8f537443fce0231adca4e9884")
        );
    }

    /// `rcv` and `fee_rcv` are private blinders and are not part of the escrow
    /// preimage.
    #[test]
    fn the_private_blinders_do_not_enter_the_digest() {
        let mut d = deposit();
        let without = deposit_digest(Address::ZERO, 1, &d);
        d.rcv += U256::from(9999);
        d.fee_rcv += U256::from(8888);
        assert_eq!(without, deposit_digest(Address::ZERO, 1, &d));
    }

    /// Every replayed field is bound, so a wrong one is caught before proving.
    #[test]
    fn every_replayed_field_changes_the_digest() {
        type Mutation = (&'static str, fn(&mut PendingDeposit));
        const MUTATIONS: &[Mutation] = &[
            ("id", |d| d.id += 1),
            ("cm", |d| d.cm[0] ^= 1),
            ("public_asset_id", |d| d.public_asset_id += 1),
            ("public_in", |d| d.public_in += 1),
            ("fee_bps_at_submit", |d| d.fee_bps_at_submit += 1),
            ("payer", |d| d.payer[0] ^= 1),
            ("submitted_at", |d| d.submitted_at += 1),
            ("cv_dep.x", |d| d.cv_dep[0] += U256::from(1)),
            ("cv_dep.y", |d| d.cv_dep[1] += U256::from(1)),
            // The fee leaf is bound for the same reason: `flushBatch` takes both
            // leaves from calldata, so a flusher able to vary these could mint
            // itself an arbitrary note.
            ("fee_in", |d| d.fee_in += 1),
            ("fee_cm", |d| d.fee_cm[0] ^= 1),
            ("fee_cv_dep.x", |d| d.fee_cv_dep[0] += U256::from(1)),
            ("fee_cv_dep.y", |d| d.fee_cv_dep[1] += U256::from(1)),
        ];

        let expected = deposit_digest(Address::ZERO, 1, &deposit());
        for (field, mutate) in MUTATIONS {
            let mut d = deposit();
            mutate(&mut d);
            assert_ne!(
                deposit_digest(Address::ZERO, 1, &d),
                expected,
                "changing {field} left the digest untouched"
            );
        }
    }

    /// The anti-replay prefix: the same deposit in another pool, or on another
    /// chain, yields a different digest.
    #[test]
    fn the_digest_is_bound_to_the_pool_and_the_chain() {
        let d = deposit();
        let here = deposit_digest(Address::from([0x11; 20]), 1, &d);
        assert_ne!(here, deposit_digest(Address::from([0x12; 20]), 1, &d));
        assert_ne!(here, deposit_digest(Address::from([0x11; 20]), 2, &d));
    }

    fn address(hex: &str) -> Address {
        hex.parse().expect("test address is valid hex")
    }
}
