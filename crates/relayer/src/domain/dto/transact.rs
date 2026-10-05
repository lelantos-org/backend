use serde::Deserialize;

/// Input/output arity of the deployed transact circuit (`transact_4x6`).
/// Mirrors `PubInputs.TRANSACT_IN` / `TRANSACT_OUT` and
/// `sdk/src/core/shape.ts :: TRANSACT_4X6`.
///
/// These are wire-format array lengths, so a value disagreeing with the deployed
/// circuit rejects every submission at the JSON boundary, where serde refuses a
/// fixed-size array of the wrong length, before the relayer can log a shape
/// problem. Moving them requires moving the `sol!` aux arity in
/// `adapters/abi.rs` and the coefficient layout in
/// `services/transact_verifier/public_signals.rs` in the
/// same change.
pub const TRANSACT_IN: usize = 4;
pub const TRANSACT_OUT: usize = 6;

/// Wallet-to-relayer wire format for the spend path. Mirrors
/// `sdk/src/protocol/transact.ts :: SubmitTransactPayload`. All
/// field-element strings are decimal (snarkjs convention); addresses are
/// 0x-hex.
///
/// The shield path is server-initiated: the relayer picks up `DepositEscrowed`
/// events from the database, and wallets do not POST deposits through this HTTP
/// surface.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubmitSpendPayload {
    pub chain_id: i64,
    pub kind: SpendKind,
    /// Groth16 proof for the deployed transact shape.
    pub proof: ProofDto,
    /// The base logical public inputs, in `PubInputs.compress(Transact)` order.
    /// The relayer derives the per-output clue slots and the aux digest from
    /// `aux`, so they are absent here; see `services::transact_verifier::public_signals` for the full
    /// coefficient count.
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
    /// Routed to `NativeAdapter.withdrawNative` rather than to MASP, which is
    /// ERC-20 only. The SNARK must name the adapter as both `recipient` and
    /// `relayer`.
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
    /// Zero on a transfer, which names no asset.
    pub public_asset_id: u64,
    pub public_out: u64,
    /// The transact circuit's digest public signal, a decimal field element.
    /// Passed to the pool as given: it is hashed into the challenge and handed
    /// to the verifier, so any other value fails the proof.
    pub digest: String,
    pub recipient: String,
    pub chain_id: u64,
    pub payer: String,
    pub relayer: String,
    /// `SwapWrapper._intentHash` or `GenericCallWrapper.intentHash`,
    /// proof-bound through the challenge.
    /// Decimal (or 0x-hex) uint256 string like the other field words. Required
    /// like its neighbours: a swap or generic call must carry the hash of its
    /// own terms (see `pipeline::swap` and `pipeline::generic`), and spends send
    /// `"0"`, which the pool ignores.
    pub intent_hash: String,
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
    /// Subgroup witness for the clue: `[8]·clue_q == clue_r`.
    pub clue_q: PointDto,
    pub eph_pub: PointDto,
    pub ciphertext: String,
}
