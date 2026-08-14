use crate::domain::dto::transact::{OutputAuxDto, ProofDto, PubInputsDto, TRANSACT_OUT};
use serde::Deserialize;

/// Wallet-to-relayer wire format for `/v1/swap`. Mirrors
/// `sdk/src/protocol/transact.ts :: SubmitSwapPayload`.
///
/// The proof + pi + aux fields are identical to a `withdraw` spend whose
/// `recipient` happens to be the SwapWrapper. The relayer reuses every
/// existing builder for those legs, then appends the swap-specific blob.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubmitSwapPayload {
    pub chain_id: i64,
    /// Leg-1 transact SNARK + PIs. `recipient` MUST equal the chain's
    /// configured `swap_wrapper_address`.
    pub proof: ProofDto,
    pub pub_inputs: PubInputsDto,
    pub aux: [OutputAuxDto; TRANSACT_OUT],
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
    /// Slim deposit request for the B note. `payer` MUST equal the
    /// `swap_wrapper_address`.
    pub deposit_d: DepositRequestDto,
    /// One leaf per deposit, hence one aux payload.
    pub aux_d: OutputAuxDto,
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

/// `PubInputs.DepositRequest` mirror. One leaf: the request used to carry a
/// second, zero-value pad leaf so a deposit matched a spend's two-leaf
/// shape; the contract collapsed it, which also removed `rcvTotal`.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DepositRequestDto {
    pub chain_id: u64,
    pub public_asset_id: u64,
    pub public_in: u64,
    pub payer: String,
    pub recipient: String,
    pub out_cm: String,
    /// Depositor-anchored Pedersen value commitment (Baby-Jubjub coords).
    /// Bound on-chain into the leaf hash via `Poseidon(TAG_LEAF, cm, x, y)`.
    pub cv_dep: [String; 2],
    /// The leaf's `rcv_dep`. Private witness for the batch circuit's
    /// per-leaf deposit binding; published off-chain via `DepositEscrowed`.
    pub rcv: String,
}
