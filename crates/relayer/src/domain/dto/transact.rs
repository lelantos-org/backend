use serde::Deserialize;

/// Input/output arity of the deployed transact circuit (`transact_3x3`).
/// Mirrors `PubInputs.TRANSACT_IN` / `TRANSACT_OUT` and
/// `sdk/src/core/shape.ts :: TRANSACT_3X3`.
pub const TRANSACT_IN: usize = 3;
pub const TRANSACT_OUT: usize = 3;

/// Wallet-to-relayer wire format for the spend path. Mirrors
/// `sdk/src/protocol/transact.ts :: SubmitTransactPayload`. All
/// field-element strings are decimal (snarkjs convention); addresses are
/// 0x-hex.
///
/// Shield path is server-initiated (relayer cron picks up `DepositEscrowed`
/// events from the DB) — wallets do NOT POST deposits through this HTTP
/// surface.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubmitSpendPayload {
    pub chain_id: i64,
    pub kind: SpendKind,
    /// Groth16 proof for the deployed transact shape (`transact_3x3`).
    pub proof: ProofDto,
    /// 27 base logical PIs in `PubInputs.compress(Transact)` order. The
    /// relayer derives the 9 clue slots and the aux digest from `aux`, so
    /// they are absent here — 42 coefficients total at 3x3.
    pub pub_inputs: PubInputsDto,
    pub aux: [OutputAuxDto; TRANSACT_OUT],
}

/// Which spend entry-point to invoke.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum SpendKind {
    #[serde(rename = "transfer")]
    Transfer,
    #[serde(rename = "withdraw")]
    Withdraw,
    /// Routed to `NativeAdapter.withdrawNative`, not to MASP — the pool is
    /// ERC-20 only. The SNARK must name the adapter as both `recipient`
    /// and `relayer`.
    #[serde(rename = "withdrawNative")]
    WithdrawNative,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProofDto {
    pub pi_a: [String; 3],
    pub pi_b: [[String; 2]; 3],
    pub pi_c: [String; 3],
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PubInputsDto {
    pub merkle_root: String,
    pub nullifier: [String; TRANSACT_IN],
    pub out_cm: [String; TRANSACT_OUT],
    pub public_asset_id: u64,
    pub public_in: u64,
    pub public_out: u64,
    pub in_cv: [PointDto; TRANSACT_IN],
    pub out_cv: [PointDto; TRANSACT_OUT],
    /// Per-output Pedersen value commitments anchored to the spender's
    /// blinders (`value_j · V^assetId + rcv_dep_j · H`). The spend SNARK
    /// rebuilds the tree leaves over these same coords, and the MASP
    /// `transfer/withdraw` entry-points cross-bind them to
    /// `tpi.cvDeps[0..TRANSACT_OUT-1]`. Wallet-supplied.
    pub out_cv_dep: [PointDto; TRANSACT_OUT],
    pub recipient: String,
    pub chain_id: u64,
    pub payer: String,
    pub relayer: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PointDto {
    pub x: String,
    pub y: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OutputAuxDto {
    pub clue_r: PointDto,
    pub eph_pub: PointDto,
    pub ciphertext: String,
}
