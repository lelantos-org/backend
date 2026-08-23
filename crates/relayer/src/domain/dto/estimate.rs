use super::transact::SpendKind;
use serde::Deserialize;

/// Wire format for `/v1/spend/estimate`.
///
/// Deliberately not `SubmitSpendPayload`. The estimate resolves to
/// `fee_quoter.quote_for_gas(gas_witness.gas_for(EntryPoint::from(kind)))` — a
/// function of `(chain_id, kind)` and nothing else. The submit payload it used
/// to take carries three nullifiers, `recipient`, `payer` and the output
/// ciphertexts, so the route accepted and parsed a real spend in exchange for a
/// number it would have returned regardless. Worse, an estimate is fired for
/// amounts a user may never submit, so those nullifiers name notes that never
/// reach the chain.
#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct EstimateSpendRequest {
    pub chain_id: i64,
    pub kind: SpendKind,
}

/// Wire format for `/v1/swap/estimate`. See [`EstimateSpendRequest`].
///
/// No `kind`: every swap prices as `EntryPoint::Swap`. The adapter and route
/// do not enter the gas witness, so they are not asked for.
#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct EstimateSwapRequest {
    pub chain_id: i64,
}
