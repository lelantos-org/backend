use crate::domain::dto::transact::{OutputAuxDto, ProofDto, PubInputsDto};
use serde::Deserialize;

/// Wallet-to-relayer wire format for `/v1/swap`. Mirrors
/// `sdk/src/relayer.ts :: SubmitSwapPayload` (added in a follow-up SDK PR).
///
/// The proof + pi + aux fields are identical to a `withdraw` spend whose
/// `recipient` happens to be the SwapWrapper. The relayer reuses every
/// existing builder for those legs, then appends the swap-specific blob.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubmitSwapPayload {
    pub chain_id: i64,
    /// Leg-1 transact_2x2 SNARK + PIs. `recipient` MUST equal the chain's
    /// configured `swap_wrapper_address`.
    pub proof2x2: ProofDto,
    pub pub_inputs: PubInputsDto,
    pub aux: [OutputAuxDto; 2],
    pub swap: SwapBlob,
}

/// Leg-2 escrow data + venue routing. No SNARK at the swap call; the
/// relayer's existing flush flow materialises the B note later.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SwapBlob {
    /// `ISwapAdapter` address. Wrapper rejects anything not on its
    /// allowlist, so the relayer does not re-validate here.
    pub adapter: String,
    /// Adapter-specific calldata blob. UniV3 single-hop is
    /// `abi.encode(uint24 fee, uint160 sqrtPriceLimitX96)` (64B); multi-hop
    /// uses `abi.encodePacked` path bytes. 0x-hex.
    pub route: String,
    /// Slim deposit intent for the B note. `payer` MUST equal the
    /// `swap_wrapper_address`.
    pub intent_d: DepositIntentDto,
    pub aux_d: [OutputAuxDto; 2],
    pub token_in: String,
    pub token_out: String,
    /// Decimal U256 string. Must equal `pi_w.publicOut * scale`; the
    /// wrapper re-asserts on-chain.
    pub amount_in: String,
    /// Decimal U256 string. Wrapper enforces `actualOut >= minOut`.
    pub min_out: String,
    /// Hard expiry, unix seconds. Wrapper reverts `SwapExpired` once
    /// `block.timestamp > deadline`. None → relayer applies its default.
    #[serde(default)]
    pub deadline: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DepositIntentDto {
    pub chain_id: u64,
    pub public_asset_id: u64,
    pub public_in: u64,
    pub payer: String,
    pub recipient: String,
    pub out_cm: [String; 2],
    /// Depositor-anchored Pedersen value commitments (Baby-Jubjub coords).
    /// Bound on-chain into the leaf hash via `Poseidon(TAG_LEAF, cm, x, y)`.
    pub cv_dep0: [String; 2],
    pub cv_dep1: [String; 2],
    /// `rcv_dep_0 + rcv_dep_1` (mod BJJ scalar order). Private witness for
    /// the relayer's per-pair deposit aggregate; published off-chain via
    /// `IntentEscrowed`.
    pub rcv_total: String,
}
