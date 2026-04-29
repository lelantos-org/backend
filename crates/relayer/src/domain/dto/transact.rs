use serde::Deserialize;

/// Wallet-to-relayer wire format for the spend path. Mirrors
/// `sdk/src/relayer.ts :: SubmitSpendPayload`. All field-element strings
/// are decimal (snarkjs convention); addresses are 0x-hex.
///
/// Shield path is server-initiated (relayer cron picks up `IntentEscrowed`
/// events from the DB) — wallets do NOT POST shield intents through this
/// HTTP surface.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubmitSpendPayload {
    pub chain_id: i64,
    pub kind: SpendKind,
    pub proof2x2: ProofDto,
    /// 22 logical PIs in `MASP._compressPubInputs` order. transact_2x2 SNARK.
    pub pub_inputs: PubInputsDto,
    pub aux: [OutputAuxDto; 2],
}

/// Which spend entry-point to invoke.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum SpendKind {
    #[serde(rename = "transfer")]
    Transfer,
    #[serde(rename = "withdraw")]
    Withdraw,
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
    pub nullifier: [String; 2],
    pub out_cm: [String; 2],
    pub public_asset_id: u64,
    pub public_in: u64,
    pub public_out: u64,
    pub in_cv: [PointDto; 2],
    pub out_cv: [PointDto; 2],
    /// Per-output Pedersen value commitments anchored to the depositor's
    /// blinding sums (`value_j · V^assetId + rcv_dep_j · H`). The spend
    /// SNARK rebuilds the tree leaves over these same coords, and the
    /// MASP `transfer/withdraw` entry-points cross-bind them to
    /// `tpi.cvDeps[0..1]`. Wallet-supplied.
    pub out_cv_dep: [PointDto; 2],
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
