use super::transact::SpendKind;
use serde::Deserialize;

/// Wire format for `/v1/spend/estimate`.
///
/// Not `SubmitSpendPayload`. The estimate resolves to
/// `fee_quoter.quote_for_gas(gas_witness.gas_for(EntryPoint::from(kind)))`, a
/// function of `(chain_id, kind)` alone. A submit payload would carry
/// nullifiers, `recipient`, `payer` and the output ciphertexts, so the route
/// would parse a real spend in exchange for a number independent of it, and an
/// estimate is fired for amounts a user may never submit.
#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct EstimateSpendRequest {
    pub chain_id: i64,
    pub kind: SpendKind,
}

/// Wire format for `/v1/deposit/estimate`. See [`EstimateSpendRequest`].
///
/// No `kind`: every deposit prices as `EntryPoint::Flush`. Unlike a spend, the
/// quote is not for a transaction the caller is about to send; a deposit is
/// escrowed by the wallet and flushed later, so what is priced is this deposit's
/// share of a future `flushBatch`.
#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct EstimateDepositRequest {
    pub chain_id: i64,
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
