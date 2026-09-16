//! Quoting and charging, identically for every pipeline.

use crate::adapters::parse::parse_address;
use crate::domain::dto::{OutputAuxDto, PubInputsDto, TRANSACT_OUT};
use crate::domain::error::AppResult;
use crate::domain::responses::{EstimateResponse, FeeQuote};
use crate::services::fees::quote::FeeQuoter;
use crate::services::fees::shielded::ShieldedFeeChecker;
use ::asset_registry::{AssetRegistry, AssetRow, Scale};
use alloy::primitives::U256;

/// One quote in, one quote per REGISTERED ASSET out.
///
/// `FeeQuoter` is keyed by ERC-20 address and the registry by asset id, and that
/// relation is one-to-many: a yield asset is registered alongside the plain
/// asset it shadows and shares its token, differing only in the venue binding.
/// Decorating each quote in place could therefore name only one of them —
/// whichever the registry happened to list first — and a client asking to pay in
/// the other is told the relayer never quoted it, so no yield id could pay a
/// fee.
///
/// The price is per token and identical across the ids sharing it. Only the id,
/// the scale and the circuit amount differ, since a yield unit is worth
/// `gross / supply` rather than `scale`.
fn decorate_fees(fees: Vec<FeeQuote>, registered: &[AssetRow]) -> Vec<FeeQuote> {
    let mut out: Vec<FeeQuote> = Vec::with_capacity(fees.len());
    for quote in fees {
        let token = parse_address(&quote.token_address).ok();
        let amount = U256::from_str_radix(&quote.amount, 10).ok();
        let (Some(token), Some(amount)) = (token, amount) else {
            // Nothing to join on. Kept undecorated for display, as an
            // unregistered token is.
            out.push(quote);
            continue;
        };

        let before = out.len();
        for row in registered
            .iter()
            .filter(|a| a.token_address() == Some(token))
        {
            // A yield asset whose index has not been polled yet yields no
            // `rate`, so it contributes no quote rather than one converted at
            // `scale` — which would name a circuit amount that underpays.
            let Some(rate) = Scale::from_decimal(&row.scale).and_then(|s| row.rate(s)) else {
                continue;
            };
            out.push(FeeQuote {
                asset_id: Some(row.asset_id_u64),
                scale: Some(row.scale.to_string()),
                circuit_amount: Some(rate.to_circuit_ceil(amount).to_string()),
                ..quote.clone()
            });
        }

        // A fee token with no registered asset — or none the indexer can price
        // yet — is left undecorated rather than dropped: the amount is still
        // useful on a chain where the indexer has not caught up and no note can
        // be built.
        if out.len() == before {
            out.push(quote);
        }
    }
    out
}

/// Everything both pipelines need in order to quote and to charge.
///
/// Spend and swap differ in which contract they target and how they encode
/// calldata, not in how a fee is priced or collected. Passing one struct keeps
/// that true: a new field is added once and reaches both paths.
#[derive(Clone, Copy)]
pub struct FeeContext<'a> {
    pub chain_id: i64,
    pub fee_quoter: &'a FeeQuoter,
    pub assets: &'a AssetRegistry,
    /// `None` on a chain that collects no fee, where the relayer pays gas from its
    /// own signer.
    pub shielded_fee: Option<&'a ShieldedFeeChecker>,
}

impl FeeContext<'_> {
    /// Quote `gas_used`, then join the result to the asset registry so a client
    /// gets everything it needs to build a fee note in one call.
    ///
    /// `FeeQuoter` prices tokens by ERC-20 address and knows nothing about MASP
    /// asset ids or scales, while the registry knows both and nothing about
    /// prices. The pipeline layer joins the two.
    pub async fn quote(&self, gas_used: u64) -> AppResult<EstimateResponse> {
        let mut estimate = self.fee_quoter.quote_for_gas(gas_used).await?;
        estimate.shielded_fee_address = self.shielded_fee.map(|c| c.address().to_string());

        let registered = self.assets.for_chain(self.chain_id).await?;
        estimate.fees = decorate_fees(estimate.fees, &registered);
        Ok(estimate)
    }

    /// Enforce the shielded fee, where this chain collects one.
    ///
    /// Runs after the proof check, so the public inputs a fee is bound to are known
    /// good, and before the tree-mirror lock, so a caller who underpays does not
    /// park every other submission on the chain.
    pub async fn charge(
        &self,
        pi: &PubInputsDto,
        aux: &[OutputAuxDto; TRANSACT_OUT],
        gas_used: u64,
    ) -> AppResult<()> {
        let Some(checker) = self.shielded_fee else {
            return Ok(());
        };
        checker.require(pi, aux, gas_used).await
    }
}

#[cfg(test)]
mod tests {
    use super::decorate_fees;
    use crate::domain::responses::FeeQuote;
    use ::asset_registry::AssetRow;

    /// A priced token, before the registry is joined to it.
    fn quote(token: u8, amount: &str) -> FeeQuote {
        FeeQuote {
            token_symbol: "mDAI".to_string(),
            token_address: format!("0x{}", hex::encode([token; 20])),
            decimals: 18,
            amount: amount.to_string(),
            asset_id: None,
            scale: None,
            circuit_amount: None,
        }
    }

    /// A plain asset: no venue, so it prices at `scale` forever.
    fn row(asset_id: i64, token: u8) -> AssetRow {
        AssetRow {
            asset_id_u64: asset_id,
            token: vec![token; 20],
            scale: "1000000000000".parse().expect("scale"),
            decimals: Some(18),
            symbol: None,
            deposit_bps: None,
            withdraw_bps: None,
            venue: None,
            gross: None,
            total_normalized: None,
            accrued_fee_normalized: None,
            halted: None,
            index_ray: None,
            perf_bps: None,
            buffer_bps: None,
            apy_bps: None,
            apy_window_s: None,
            apy_measured_at: None,
            vault_name: None,
        }
    }

    /// The yield id registered alongside `row`'s asset, sharing its token. Its
    /// venue has earned, so a unit is worth `1.1 * scale`.
    fn yield_row(asset_id: i64, token: u8) -> AssetRow {
        AssetRow {
            venue: Some(vec![0xaa; 20]),
            gross: Some("1100000000000000".parse().expect("gross")),
            total_normalized: Some("1000".parse().expect("supply")),
            accrued_fee_normalized: Some("0".parse().expect("fee")),
            halted: Some(false),
            index_ray: Some("1100000000000000000000000000".parse().expect("index")),
            ..row(asset_id, token)
        }
    }

    fn ids(fees: &[FeeQuote]) -> Vec<Option<i64>> {
        fees.iter().map(|f| f.asset_id).collect()
    }

    /// The regression this function exists for.
    ///
    /// A yield id shares its plain id's ERC-20, so a join that took the first
    /// registered asset at that address quoted only the plain one. A wallet
    /// depositing into the yield id was then told the relayer had quoted no
    /// amount for it, so its fee note could not be built in that id at all.
    #[test]
    fn both_ids_sharing_a_token_are_quoted() {
        let registered = [row(2, 0xbb), yield_row(5, 0xbb)];
        let out = decorate_fees(vec![quote(0xbb, "1100000000000000")], &registered);

        assert_eq!(ids(&out), vec![Some(2), Some(5)]);
        // One price, two conversions: the yield unit is worth more, so covering
        // the same cost takes fewer of them.
        let plain: u128 = out[0]
            .circuit_amount
            .as_ref()
            .expect("plain")
            .parse()
            .expect("n");
        let earning: u128 = out[1]
            .circuit_amount
            .as_ref()
            .expect("yield")
            .parse()
            .expect("n");
        assert!(earning < plain, "a yield unit covers more than a plain one");
        assert_eq!(out[0].amount, out[1].amount, "the token price is the same");
    }

    /// The single-asset case is unchanged: one quote in, one out.
    #[test]
    fn a_token_with_one_asset_yields_one_quote() {
        let out = decorate_fees(vec![quote(0xbb, "1000")], &[row(2, 0xbb)]);
        assert_eq!(ids(&out), vec![Some(2)]);
    }

    /// An unregistered token keeps its quote, undecorated: the amount is still
    /// worth displaying where the indexer has not caught up.
    #[test]
    fn an_unregistered_token_is_kept_undecorated() {
        let out = decorate_fees(vec![quote(0xcc, "1000")], &[row(2, 0xbb)]);
        assert_eq!(ids(&out), vec![None]);
        assert_eq!(out.len(), 1);
    }

    /// A yield asset the poller has not reached has no rate, so it contributes
    /// no quote — but the plain id at the same address still does.
    #[test]
    fn an_unpolled_yield_asset_is_skipped_not_fatal() {
        let mut unpolled = yield_row(5, 0xbb);
        unpolled.gross = None;
        let out = decorate_fees(vec![quote(0xbb, "1000")], &[row(2, 0xbb), unpolled]);
        assert_eq!(ids(&out), vec![Some(2)]);
    }

    /// When the only asset at an address cannot be priced, the quote survives
    /// undecorated rather than vanishing from the response.
    #[test]
    fn a_token_whose_only_asset_is_unpriceable_keeps_its_quote() {
        let mut unpolled = yield_row(5, 0xbb);
        unpolled.gross = None;
        let out = decorate_fees(vec![quote(0xbb, "1000")], &[unpolled]);
        assert_eq!(ids(&out), vec![None]);
    }
}
