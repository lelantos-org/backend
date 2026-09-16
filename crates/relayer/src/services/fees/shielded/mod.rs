//! Collecting a shielded fee: recognising the notes a payer attaches to a
//! submission (`recipient`, `deposit_note`) and pricing them against what the
//! relayer quotes (this module).

pub mod deposit_note;
mod recipient;

pub use recipient::{FeeRecipient, Payment};

use crate::app::config::{BPS_DENOMINATOR, ShieldedFeeSettings};
use crate::domain::dto::{OutputAuxDto, PubInputsDto, TRANSACT_OUT};
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{ShieldedFeeOut, TokenOut};
use crate::services::fees::quote::{FeeQuoter, FeeToken};
use ::asset_registry::{AssetRegistry, AssetRow, Rate, Scale};
use alloy::primitives::U256;
use crypto::tree::Field;
use std::fmt;
use std::sync::Arc;
use tracing::info;

/// Which assets this relayer will take as a fee, and what they are worth.
///
/// Two gates, combined with AND: the operator's explicit allowlist, and whether
/// `accepted_fee_tokens` can price the token. An empty allowlist reads as
/// everything, but an asset the fee table cannot price has no quote to check a
/// payment against.
///
/// One type, because `/chains` and the submit path ask the same question and an
/// answer differing between them would advertise a fee that then 402s after the
/// wallet has built and proved the spend.
struct FeePolicy {
    /// Empty means no explicit restriction rather than everything; the fee table
    /// still decides.
    allowlist: Vec<u64>,
    fee_quoter: Arc<FeeQuoter>,
}

impl FeePolicy {
    fn allowlists(&self, asset_id: u64) -> bool {
        self.allowlist.is_empty() || self.allowlist.contains(&asset_id)
    }

    /// The fee-table entry that prices `asset`, if there is one.
    ///
    /// The registry keys on a MASP asset id and the fee table keys on an ERC-20
    /// address, so the token address is the join between them.
    fn price_for(&self, asset: &AssetRow) -> Option<&FeeToken> {
        self.fee_quoter.token_at(&asset.token_address()?)
    }

    /// Whether a registered asset can actually pay a fee.
    fn accepts(&self, asset: &AssetRow) -> bool {
        self.allowlists(asset.asset_id()) && self.price_for(asset).is_some()
    }

    /// The subset of `registered` a payer may use. What `/chains` publishes.
    fn payable<'a>(&'a self, registered: &'a [AssetRow]) -> impl Iterator<Item = &'a AssetRow> {
        registered.iter().filter(|a| self.accepts(a))
    }
}

/// Recognition plus pricing: what a chain needs to actually charge.
pub struct ShieldedFeeChecker {
    chain_id: i64,
    recipient: FeeRecipient,
    grace_bps: u32,
    policy: FeePolicy,
    assets: Arc<AssetRegistry>,
}

/// See [`FeeRecipient`]'s `Debug`: this reaches the key through `recipient`.
impl fmt::Debug for ShieldedFeeChecker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShieldedFeeChecker")
            .field("chain_id", &self.chain_id)
            .field("recipient", &self.recipient)
            .field("grace_bps", &self.grace_bps)
            .field("allowlist", &self.policy.allowlist)
            .finish_non_exhaustive()
    }
}

impl ShieldedFeeChecker {
    /// Build a checker for one chain.
    ///
    /// Fails when the configured address and viewing key do not describe the
    /// same identity — see [`FeeRecipient::new`].
    pub fn new(
        chain_id: i64,
        settings: ShieldedFeeSettings<'_>,
        ivk: Field,
        fee_quoter: Arc<FeeQuoter>,
        assets: Arc<AssetRegistry>,
    ) -> AppResult<Self> {
        let recipient = FeeRecipient::new(settings.address.to_string(), ivk)
            .map_err(|e| AppError::Internal(format!("chain {chain_id}: {e}")))?;
        Ok(Self {
            chain_id,
            recipient,
            grace_bps: settings.grace_bps,
            policy: FeePolicy {
                allowlist: settings.assets.to_vec(),
                fee_quoter,
            },
            assets,
        })
    }

    pub fn address(&self) -> &str {
        self.recipient.address()
    }

    /// The identity to trial-decrypt with, for the deposit fee leaf.
    pub fn recipient(&self) -> &FeeRecipient {
        &self.recipient
    }

    /// What one deposit must pay to be worth flushing, in circuit units of
    /// `asset_id`.
    ///
    /// Rounded up to a whole circuit unit and reduced by the configured grace,
    /// mirroring the spend path: a note value is a whole unit, so rounding down
    /// would set a bar no payer could hit exactly.
    pub async fn deposit_fee_required(&self, asset_id: u64, gas_used: u64) -> AppResult<u64> {
        let (rate, fee_token) = self.priced_asset(asset_id).await?;
        let required = self
            .policy
            .fee_quoter
            .required_amount(&fee_token, gas_used)
            .await?;
        let circuit = rate.to_circuit_ceil(apply_grace(required, self.grace_bps));
        // A required amount past 64 bits cannot be paid: the circuit range-checks
        // a note's value to 64 bits. Saturating rather than erroring keeps this a
        // fee decision, leaving every deposit short, instead of aborting the tick
        // for the rest of the batch.
        Ok(u64::try_from(circuit).unwrap_or(u64::MAX))
    }

    /// The terms `/chains` publishes.
    ///
    /// Only assets the indexer has registered and `FeePolicy` accepts appear. An
    /// unregistered one carries no `scale`, without which a client cannot turn a
    /// quoted amount into a note value, and an unpriced one would be refused on
    /// submission.
    pub fn terms(&self, registered: &[AssetRow]) -> ShieldedFeeOut {
        ShieldedFeeOut {
            address: self.address().to_string(),
            grace_bps: self.grace_bps,
            markup_bps: self.policy.fee_quoter.markup_bps,
            tokens: self.policy.payable(registered).map(TokenOut::new).collect(),
        }
    }

    /// Refuse the submission unless its outputs pay this relayer enough to
    /// cover `gas_used`.
    ///
    /// Callers source `gas_used` from the gas witness, exactly as
    /// `FeeQuoter::quote_for_gas` does, so the fee a payer is held to is the
    /// one `/v1/spend/estimate` would have quoted them.
    ///
    /// Called after the transact proof is verified and before the tree-mirror lock
    /// is taken, so a refusal costs neither a Groth16 nor another submission's
    /// turn.
    pub async fn require(
        &self,
        pi: &PubInputsDto,
        aux: &[OutputAuxDto; TRANSACT_OUT],
        gas_used: u64,
    ) -> AppResult<()> {
        let Some(payment) = self.recipient.find_payment(pi, aux)? else {
            return Err(AppError::ShieldedFeeMissing {
                address: self.address().to_string(),
            });
        };
        let (rate, fee_token) = self.priced_asset(payment.asset_id).await?;
        let paid = rate.to_base(payment.circuit_total);
        let required = self
            .policy
            .fee_quoter
            .required_amount(&fee_token, gas_used)
            .await?;
        let floor = apply_grace(required, self.grace_bps);

        if paid < floor {
            return Err(AppError::ShieldedFeeTooLow {
                asset_id: payment.asset_id,
                required: required.to_string(),
                paid: paid.to_string(),
                grace_bps: self.grace_bps,
            });
        }

        // Both sides of the decision, at the moment it is made. A refusal already
        // carries these in `ShieldedFeeTooLow`; logging an accepted fee records
        // whether relaying is paying for itself without reproducing the quote from
        // the gas figure logged separately.
        //
        // `paid` and `required` are base units of `asset_id`, and `circuit_total`
        // is what the payer's note carried. None of it identifies the payer, who
        // is not linked to the note even by this relayer, and no output index is
        // logged: it would say which slot of which submission holds the note.
        info!(
            chain_id = self.chain_id,
            asset_id = payment.asset_id,
            paid = %paid,
            required = %required,
            floor = %floor,
            grace_bps = self.grace_bps,
            circuit_total = payment.circuit_total,
            gas_used,
            "shielded fee accepted"
        );
        Ok(())
    }

    /// Resolve an asset to the two things pricing needs: its rate and the
    /// fee-table entry that gives it a price.
    ///
    /// Every failure here gives the payer the same answer, that this relayer will
    /// not take that asset, so they share one error. Which reason applies is an
    /// operator concern and appears in the message.
    ///
    /// Returns a [`Rate`] rather than a [`Scale`] because a yield asset's unit
    /// is worth `gross / supply`, not `scale`. Pricing one at `scale` demands
    /// more units than the gas costs and then values the payment at less than
    /// the pool would hand over, so a wallet that converts correctly is refused
    /// as underpaid.
    async fn priced_asset(&self, asset_id: u64) -> AppResult<(Rate, FeeToken)> {
        if !self.policy.allowlists(asset_id) {
            return Err(self.unaccepted(asset_id, "it is not in shielded_fee_assets"));
        }
        let asset = self
            .assets
            .by_asset_id(self.chain_id, asset_id)
            .await?
            .ok_or_else(|| self.unaccepted(asset_id, "it is not a registered asset"))?;
        let scale = Scale::from_decimal(&asset.scale).ok_or_else(|| {
            AppError::Internal(format!(
                "chain {}: asset {asset_id} has an unusable scale {}",
                self.chain_id, asset.scale
            ))
        })?;
        let rate = asset.rate(scale).ok_or_else(|| {
            self.unaccepted(
                asset_id,
                "it is a yield asset whose index has not been indexed yet",
            )
        })?;
        let fee_token = self
            .policy
            .price_for(&asset)
            .ok_or_else(|| {
                self.unaccepted(
                    asset_id,
                    "it is not in this relayer's accepted_fee_tokens, so it has no price",
                )
            })?
            .clone();
        Ok((rate, fee_token))
    }

    fn unaccepted(&self, asset_id: u64, why: &str) -> AppError {
        AppError::ShieldedFeeAssetRejected {
            asset_id,
            reason: why.to_string(),
        }
    }
}

/// The floor a payment must clear: the quote less the grace band.
///
/// `grace_bps` is bounded below `BPS_DENOMINATOR` by config validation, so this
/// cannot reduce the floor to nothing.
fn apply_grace(required: U256, grace_bps: u32) -> U256 {
    required * U256::from(BPS_DENOMINATOR - grace_bps) / U256::from(BPS_DENOMINATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Both directions of the mispricing a yield asset used to cause.
    ///
    /// Before `Rate`, this path converted with `scale` alone: it demanded
    /// `required / scale` units where `required / (scale * index)` covers the
    /// cost, and then valued a payment at `paid * scale` instead of
    /// `paid * scale * index`. The two compound — the relayer asks for more than
    /// it needs and credits less than it was given — so a wallet converting
    /// correctly is refused as underpaid, while one repeating the same stale
    /// error appears to work.
    #[test]
    fn a_yield_asset_prices_off_its_index_not_its_scale() {
        let scale = Scale::from_decimal(&row(1, 1).scale).expect("scale");
        let plain = row(1, 1).rate(scale).expect("a plain asset always prices");
        let earning = yield_row(2, 2)
            .rate(scale)
            .expect("a polled yield asset prices");

        // Covering a fixed gas cost takes fewer units than `scale` demands.
        let cost = U256::from(1_100_000_000_000_000u64);
        assert!(
            earning.to_circuit_ceil(cost) < plain.to_circuit_ceil(cost),
            "pricing at scale over-demands units"
        );

        // And the same units are worth more than `scale` credits.
        assert!(
            earning.to_base(1_000) > plain.to_base(1_000),
            "pricing at scale under-credits the payment"
        );
    }

    /// A yield asset the poller has not reached yet cannot be priced.
    ///
    /// `scale` is not a conservative fallback here — it is wrong by whatever the
    /// venue has already earned — so the row yields no rate and the caller
    /// declines rather than quoting.
    #[test]
    fn a_yield_asset_without_an_index_refuses_to_price() {
        let scale = Scale::from_decimal(&row(1, 1).scale).expect("scale");
        let mut unpolled = yield_row(2, 2);
        unpolled.gross = None;
        assert!(unpolled.rate(scale).is_none());
    }

    /// A yield asset whose venue has earned, so a unit is worth more than
    /// `scale`. `gross / supply` is `1.1 * scale`.
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

    fn fee_token(token: u8) -> crate::app::config::FeeTokenCfg {
        crate::app::config::FeeTokenCfg {
            symbol: format!("T{token}"),
            address: format!("0x{}", hex::encode([token; 20])),
            decimals: 18,
            quote_symbol: "USDC".to_string(),
        }
    }

    /// A policy over a real `FeeQuoter`, so these tests exercise the shipped rule
    /// rather than a stand-in.
    ///
    /// Neither the oracle nor the RPC endpoint is contacted: `token_at` reads the
    /// configured table and `RpcEndpoint::new` only parses a URL.
    fn policy(allowlist: &[u64], priced: &[u8]) -> FeePolicy {
        struct NoOracle;
        #[async_trait::async_trait]
        impl crate::services::fees::oracle::PriceOracle for NoOracle {
            async fn price(&self, _: &str, _: &str) -> AppResult<f64> {
                unreachable!("the fee table is read directly; no pricing happens here")
            }
        }
        FeePolicy {
            allowlist: allowlist.to_vec(),
            fee_quoter: Arc::new(FeeQuoter {
                chain_id: 31337,
                native_symbol: "ETH".to_string(),
                native_decimals: 18,
                accepted_fee_tokens: priced
                    .iter()
                    .map(|t| FeeToken::from_cfg(&fee_token(*t)).expect("fee token"))
                    .collect(),
                oracle: Arc::new(NoOracle),
                gas_estimator: Arc::new(crate::services::fees::gas_estimator::GasEstimator::new(
                    31337,
                    crate::adapters::rpc::endpoint("http://127.0.0.1:1").expect("url"),
                )),
                markup_bps: 1000,
            }),
        }
    }

    fn payable_ids(policy: &FeePolicy, registered: &[AssetRow]) -> Vec<u64> {
        policy.payable(registered).map(AssetRow::asset_id).collect()
    }

    /// An empty allowlist reads as everything, but the fee table must still be
    /// able to price the asset.
    ///
    /// An operator who lists WETH and USDC in `accepted_fee_tokens` and leaves
    /// `shielded_fee_assets` unset must not see DAI advertised in `/chains`, or a
    /// wallet would build and prove a whole spend before the submit refused it.
    #[test]
    fn an_asset_the_fee_table_cannot_price_is_not_advertised() {
        let registered = [row(1, 0xaa), row(2, 0xbb), row(3, 0xcc)];
        let ids = payable_ids(&policy(&[], &[0xaa, 0xbb]), &registered);
        assert_eq!(ids, vec![1, 2], "asset 3 has no price and must not appear");
    }

    #[test]
    fn the_allowlist_narrows_the_priced_set_further() {
        let registered = [row(1, 0xaa), row(2, 0xbb)];
        assert_eq!(
            payable_ids(&policy(&[2], &[0xaa, 0xbb]), &registered),
            vec![2]
        );
    }

    /// Both gates are combined with AND: allowlisting an asset does not supply a
    /// price.
    #[test]
    fn allowlisting_an_unpriced_asset_does_not_advertise_it() {
        let registered = [row(3, 0xcc)];
        assert!(payable_ids(&policy(&[3], &[0xaa]), &registered).is_empty());
    }

    /// A row whose address column is not 20 bytes must drop out rather than panic;
    /// the column is written by another service.
    #[test]
    fn a_row_with_a_malformed_token_address_is_skipped() {
        let mut bad = row(1, 0xaa);
        bad.token = vec![0xaa; 19];
        assert!(bad.token_address().is_none());
        assert!(payable_ids(&policy(&[], &[0xaa]), &[bad]).is_empty());
    }

    #[test]
    fn the_grace_band_lowers_the_floor_by_exactly_its_share() {
        let required = U256::from(10_000u64);
        assert_eq!(apply_grace(required, 0), required);
        assert_eq!(apply_grace(required, 300), U256::from(9_700u64));
        assert_eq!(apply_grace(required, 5_000), U256::from(5_000u64));
    }
}
