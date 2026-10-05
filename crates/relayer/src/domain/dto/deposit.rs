use serde::Deserialize;

/// Mirror of `PubInputs.DepositRequest`.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DepositRequestDto {
    pub chain_id: u64,
    pub public_asset_id: u64,
    pub public_in: u64,
    pub payer: String,
    pub recipient: String,
    /// `Poseidon(TAG_INNER, pk, rho, rcm)` of the deposited note, 0x-hex. The
    /// batch circuit builds the leaf from it and the public amount.
    pub inner: String,
    /// The deposit's second leaf: a note paying whoever flushes the batch.
    ///
    /// On a wrapper path this is a zero-value pad, since the submission already
    /// pays the relayer on its withdraw leg, but the leaf is still minted and
    /// still escrow digest preimage, so the fields must be carried.
    ///
    /// `fee_asset_id` is 0 when `fee_in` is 0. A valued fee note must be in
    /// `public_asset_id`, since a wrapper escrows only that token.
    pub fee_asset_id: u64,
    pub fee_in: u64,
    /// `inner` of the fee note, 0x-hex.
    pub fee_inner: String,
}
