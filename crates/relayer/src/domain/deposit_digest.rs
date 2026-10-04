//! Off-chain twin of `MASP._depositDigest`.
//!
//! The contract stores a deposit as a single keccak digest and drops every field
//! that went into it. `flushBatch` re-derives the digest from the `DepositMeta`
//! the relayer replays and reverts `DigestMismatch(id)` for the whole batch if it
//! differs. Deriving it here lets the flush pipeline drop a deposit whose
//! replayed fields cannot match before paying for a `tree_update_batch` Groth16.
//!
//! Must stay byte-identical to `contracts/src/MASP.sol::_depositDigest`.

use crate::domain::deposit::PendingDeposit;
use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::sol_types::SolValue;

/// The contract narrows `publicIn` to `uint48` after bounding it, so a wider
/// value both hashes differently and reverts `PublicInTooLarge` first.
pub const MAX_PUBLIC_IN: u64 = (1u64 << 48) - 1;

/// `keccak256(abi.encode(address(this), block.chainid, id, inner,
/// publicAssetId, publicIn, feeBpsAtSubmit, payer, submittedAt, feeNote,
/// pulled))`: 13 words. `feeNote` is the static struct `PubInputs.FeeNote`,
/// which encodes in place as `feeIn`, `feeAssetId`, `feeInner`.
///
/// `abi.encode` is not packed, so every static field occupies a full word and the
/// declared Solidity widths (`uint64`, `uint48`, `uint16`, `uint32`) encode
/// identically to `U256` provided the value fits, which [`MAX_PUBLIC_IN`] and the
/// `PendingDeposit` field types guarantee.
///
/// The three fields from `feeIn` bind the relayer's fee note, and are preimage
/// for the same reason the depositor's leaf is: `flushBatch` supplies both leaves
/// from calldata, so a flusher able to vary them could mint itself an arbitrary
/// note. `feeAssetId` ties the fee leaf's `leafAsset` to the token pulled at
/// submit, which need not be the deposit's.
///
/// The last word is the escrow's refund cap, bound so that a canceller cannot
/// choose it; see [`PendingDeposit::pulled`].
pub fn deposit_digest(masp: Address, chain_id: u64, d: &PendingDeposit) -> B256 {
    let preimage = (
        masp,
        U256::from(chain_id),
        U256::from(d.id),
        B256::from(d.inner),
        U256::from(d.public_asset_id),
        U256::from(d.public_in),
        U256::from(d.fee_bps_at_submit),
        Address::from(d.payer),
        U256::from(d.submitted_at),
        U256::from(d.fee_in),
        U256::from(d.fee_asset_id),
        B256::from(d.fee_inner),
        d.pulled,
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
            inner: [0xab; 32],
            public_asset_id: 2,
            public_in: 3,
            fee_bps_at_submit: 4,
            payer: [0xcd; 20],
            submitted_at: 5,
            fee_asset_id: 13,
            fee_in: 9,
            fee_inner: [0xef; 32],
            fee_aux: JsonValue::Null,
            pulled: U256::from(14),
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
    ///   'f(address,uint256,uint256,bytes32,uint64,uint48,uint16,address,uint32,\
    ///      (uint48,uint64,bytes32),uint256)' \
    ///   0x1111111111111111111111111111111111111111 31337 42 \
    ///   0x2222222222222222222222222222222222222222222222222222222222222222 \
    ///   7 1000000 25 0x3333333333333333333333333333333333333333 123456 \
    ///   '(9,8,0x4444444444444444444444444444444444444444444444444444444444444444)' 10)"
    /// ```
    ///
    /// Drift here quarantines every deposit on every chain, so this and the test
    /// below pin against external encoders rather than against `deposit_digest`.
    #[test]
    fn the_digest_matches_the_contract_encoding() {
        let d = PendingDeposit {
            id: 42,
            inner: [0x22; 32],
            public_asset_id: 7,
            public_in: 1_000_000,
            fee_bps_at_submit: 25,
            payer: [0x33; 20],
            submitted_at: 123_456,
            // Distinct from `public_asset_id`: a cross-asset fee note.
            fee_asset_id: 8,
            fee_in: 9,
            fee_inner: [0x44; 32],
            fee_aux: JsonValue::Null,
            pulled: U256::from(10),
        };
        assert_eq!(
            deposit_digest(Address::from([0x11; 20]), 31337, &d),
            b256("0xabae3c51156949b837ca004e7836a088f148e37ecc9520dfd46e813eb01669f2")
        );
    }

    /// The same check against a real deployment rather than an encoder: a
    /// `MASP.deposit()` in the Foundry harness
    /// (`contracts/test/masp/MASP.escrowEventRoundtrip.t.sol`,
    /// `test_cancelDeposit_reconstructedFromEventOnly`), with the preimage taken
    /// from the `DepositEscrowed` log as the indexer takes it and `submittedAt`
    /// from the emitting block. Catches a field the relayer sources from the wrong
    /// column, which the encoder test cannot.
    ///
    /// The pool is behind a `DelayedUpgradeProxy`, so `address(this)` is the proxy
    /// (`0xa0Cb…7598`), and the digest is what the deposit stored in
    /// `escrowed[0]`, as read from the `forge test -vvvvv` trace.
    #[test]
    fn the_digest_matches_a_deposit_a_deployed_masp_escrowed() {
        let d = PendingDeposit {
            id: 0,
            inner: U256::from(0x111).to_be_bytes(),
            public_asset_id: 1,
            public_in: 100,
            fee_bps_at_submit: 25,
            payer: address("0x000000000000000000000000000000000000Face").into(),
            submitted_at: 1,
            // The harness deposits with a zero-value fee note, a valid shape: a
            // subsidised chain sets `feeIn` to zero and the leaf is still minted
            // and spendable.
            fee_asset_id: 0,
            fee_in: 0,
            fee_inner: U256::from(0xfee).to_be_bytes(),
            fee_aux: JsonValue::Null,
            // The asset is plain, so the log carries no refund cap.
            pulled: U256::ZERO,
        };
        assert_eq!(
            deposit_digest(
                address("0xa0Cb889707d426A7A386870A03bc70d1b0697598"),
                31337,
                &d
            ),
            b256("0x66fbdace2fa43c7ebbf5d80a794383686b245600484d8421ec230c395b381c63")
        );
    }

    /// A deployed cross-asset deposit: the fee note is paid in asset 2 while the
    /// deposit is in asset 1, through the two-token Permit2 batch
    /// (`contracts/test/masp/MASP.depositFeeAsset.t.sol`,
    /// `test_signature_crossAsset_pullsBothTokensAndBindsFeeAsset`, which asserts
    /// this digest against its own `abi.encode`). Values from the
    /// `DepositEscrowed` log and `escrowed(0)` in that test's `-vvvvv` trace.
    #[test]
    fn the_digest_matches_a_cross_asset_deposit_a_deployed_masp_escrowed() {
        let d = PendingDeposit {
            id: 0,
            inner: U256::from(0x100).to_be_bytes(),
            public_asset_id: 1,
            public_in: 1_000,
            fee_bps_at_submit: 25,
            payer: address("0x5F3cAc19f89bd2e972062Db0F287c525E1913341").into(),
            submitted_at: 1,
            fee_asset_id: 2,
            fee_in: 7,
            fee_inner: U256::from(0x101).to_be_bytes(),
            fee_aux: JsonValue::Null,
            pulled: U256::ZERO,
        };
        assert_eq!(
            deposit_digest(
                address("0xa0Cb889707d426A7A386870A03bc70d1b0697598"),
                31337,
                &d
            ),
            b256("0x178a6da8c45747a816048ca5d234358899c19e550db7e854a19efaf145a91aab")
        );
    }

    /// A deployed yield escrow: asset 9 is bound to a venue, so the pool logs
    /// what it pulled at submit and binds it as the digest's last word
    /// (`contracts/test/yield/YieldEscrow.t.sol`,
    /// `test_revert_cap_isBoundByTheDigest`). Values from the `DepositEscrowed`
    /// log and `escrowed(0)` in that test's `-vvvvv` trace.
    ///
    /// Nothing else in the row determines the cap: it is 1e6 units plus the 25 bps
    /// fee, priced at the venue's index in the submit block. Replayed as a plain
    /// asset's 0 the digest differs, which is the `DigestMismatch` that test
    /// expects of a flush.
    #[test]
    fn the_digest_matches_a_yield_deposit_a_deployed_masp_escrowed() {
        let d = PendingDeposit {
            id: 0,
            inner: U256::from(0x101).to_be_bytes(),
            public_asset_id: 9,
            public_in: 1_000_000,
            fee_bps_at_submit: 25,
            payer: address("0x00000000000000000000000000000000000A11cE").into(),
            submitted_at: 1,
            fee_asset_id: 0,
            fee_in: 0,
            fee_inner: U256::from(0x102).to_be_bytes(),
            fee_aux: JsonValue::Null,
            pulled: U256::from(10_025_000_000_000_000u64),
        };
        let masp = address("0xa0Cb889707d426A7A386870A03bc70d1b0697598");
        let escrowed = b256("0xfba2693295504b3e98cdc1053ee8f38ac512811e52d6e102e942ee514bf23578");
        assert_eq!(deposit_digest(masp, 31337, &d), escrowed);

        let without_the_cap = PendingDeposit {
            pulled: U256::ZERO,
            ..d
        };
        assert_ne!(deposit_digest(masp, 31337, &without_the_cap), escrowed);
    }

    /// Every replayed field is bound, so a wrong one is caught before proving.
    #[test]
    fn every_replayed_field_changes_the_digest() {
        type Mutation = (&'static str, fn(&mut PendingDeposit));
        const MUTATIONS: &[Mutation] = &[
            ("id", |d| d.id += 1),
            ("inner", |d| d.inner[0] ^= 1),
            ("public_asset_id", |d| d.public_asset_id += 1),
            ("public_in", |d| d.public_in += 1),
            ("fee_bps_at_submit", |d| d.fee_bps_at_submit += 1),
            ("payer", |d| d.payer[0] ^= 1),
            ("submitted_at", |d| d.submitted_at += 1),
            // The fee leaf is bound for the same reason: `flushBatch` takes both
            // leaves from calldata, so a flusher able to vary these could mint
            // itself an arbitrary note.
            ("fee_in", |d| d.fee_in += 1),
            // The fee asset is chosen by the payer and may differ from the
            // deposit's, so a flusher able to vary it could claim a note in a
            // token that was never pulled.
            ("fee_asset_id", |d| d.fee_asset_id += 1),
            ("fee_inner", |d| d.fee_inner[0] ^= 1),
            // The refund cap: a canceller able to vary it could raise its own
            // ceiling, and nothing but the digest holds it.
            ("pulled", |d| d.pulled += U256::from(1)),
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
