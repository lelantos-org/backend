use crate::domain::dto::transact::{OutputAuxDto, ProofDto, PubInputsDto, TRANSACT_OUT};
use serde::Deserialize;

/// Wallet-to-relayer wire format for `/v1/swap`. Mirrors
/// `sdk/src/protocol/transact.ts :: SubmitSwapPayload`.
///
/// The proof, public-input and aux fields are identical to a `withdraw` spend
/// whose `recipient` is the SwapWrapper. The relayer reuses the existing builders
/// for those legs and appends the swap-specific blob.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubmitSwapPayload {
    pub chain_id: i64,
    /// Leg-1 transact SNARK and public inputs. `recipient` must equal the chain's
    /// configured `swap_wrapper_address`.
    pub proof: ProofDto,
    pub pub_inputs: PubInputsDto,
    pub aux: [OutputAuxDto; TRANSACT_OUT],
    pub swap: SwapBlob,
}

/// Leg-2 escrow data and venue routing. No SNARK at the swap call; the relayer's
/// flush flow materialises the B note later.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SwapBlob {
    /// `ISwapAdapter` address. The wrapper rejects anything off its allowlist, so
    /// the relayer does not re-validate here.
    pub adapter: String,
    /// Adapter-specific calldata blob, 0x-hex. UniV3 single-hop is
    /// `abi.encode(uint24 fee, uint160 sqrtPriceLimitX96)`, 64 bytes; multi-hop
    /// uses `abi.encodePacked` path bytes.
    pub route: String,
    /// Deposit request for the B note. `payer` must equal the
    /// `swap_wrapper_address`.
    pub deposit_d: DepositRequestDto,
    /// The B-note deposit's own leaf.
    pub aux_d: OutputAuxDto,
    /// The B-note deposit's fee leaf. Two leaves per deposit means two aux
    /// payloads. The swap pays the relayer on its withdraw leg, so this carries a
    /// zero-value note, which is still a leaf and still digest preimage.
    pub fee_aux_d: OutputAuxDto,
    pub token_in: String,
    pub token_out: String,
    /// Decimal U256 string. Must equal `pi_w.publicOut * scale`; the wrapper
    /// re-asserts this on-chain.
    pub amount_in: String,
    /// Decimal U256 string. Wrapper enforces `actualOut >= minOut`.
    pub min_out: String,
    /// Hard expiry in unix seconds. The wrapper reverts `SwapExpired` once
    /// `block.timestamp > deadline`. `None` applies the relayer's default.
    #[serde(default)]
    pub deadline: Option<String>,
}

/// Mirror of `PubInputs.DepositRequest`.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DepositRequestDto {
    pub chain_id: u64,
    pub public_asset_id: u64,
    pub public_in: u64,
    pub payer: String,
    pub recipient: String,
    pub out_cm: String,
    /// Depositor-anchored Pedersen value commitment, in Baby-Jubjub coordinates.
    /// Bound on-chain into the leaf hash via `Poseidon(TAG_LEAF, cm, x, y)`.
    pub cv_dep: [String; 2],
    /// The leaf's `rcv_dep`. Private witness for the batch circuit's
    /// per-leaf deposit binding; published off-chain via `DepositEscrowed`.
    pub rcv: String,
    /// The deposit's second leaf: a note paying whoever flushes the batch.
    ///
    /// On the swap path this is a zero-value pad, since the swap already pays the
    /// relayer on its withdraw leg, but the leaf is still minted and still escrow
    /// digest preimage, so the fields must be carried.
    pub fee_in: u64,
    pub fee_cm: String,
    pub fee_cv_dep: [String; 2],
    pub fee_rcv: String,
}
