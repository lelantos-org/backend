use serde::Serialize;

// `Clone` so one priced token can be fanned out into a quote per asset id
// registered at its address — see `FeeContext::quote`.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FeeQuote {
    pub token_symbol: String,
    pub token_address: String,
    pub decimals: u8,
    /// Base-unit U256 as decimal string.
    pub amount: String,
    /// MASP asset id, present once the indexer has registered this token.
    ///
    /// `null` means the relayer cannot yet map this fee token to an asset, so a
    /// client cannot build a fee note for it. It does not mean the token is
    /// unpriced; [`Self::amount`] is still meaningful for display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<i64>,
    /// `baseUnits = circuitUnits * scale`, decimal string. Absent alongside
    /// [`Self::asset_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    /// [`Self::amount`] rounded up to a whole circuit unit: the exact `value` to
    /// put in the fee note.
    ///
    /// Rounded here rather than by the client, since rounding down would underpay
    /// by up to one whole unit and be refused, and two implementations of the same
    /// rounding would drift. Absent alongside [`Self::asset_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circuit_amount: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateResponse {
    pub gas_used: u64,
    pub effective_gas_price_wei: String,
    pub total_native_wei: String,
    /// Per-chain markup applied (bps; 1000 = 10%).
    pub markup_bps: u32,
    /// Unix seconds (server time) when this quote was produced.
    pub quoted_at: u64,
    pub fees: Vec<FeeQuote>,
    /// Where to send the fee note, when this chain collects a shielded fee.
    ///
    /// Absent means the relayer is not charging on this chain, and a spend with no
    /// fee output is still relayed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shielded_fee_address: Option<String>,
}
