//! One registered asset, joined across `assets` and `asset_yield`.

use crate::numeric::bigdecimal_to_u256;
use crate::units::{Rate, Scale};
use alloy::primitives::Address;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;

/// One asset's estimated rate, and the window it was measured over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApyEstimate {
    /// Annualized, in basis points, net of the pool's performance fee and
    /// buffer. Negative after a venue loss, which is a real outcome.
    pub bps: i32,
    /// Seconds actually spanned by the two readings — not the window that was
    /// aimed for.
    pub window_s: i64,
}

/// How old a stored estimate may be and still be published.
///
/// Takes over from the TTL of the process-local cache this used to live in, and
/// keeps its reasoning: the worker re-measures every 30 minutes, and entries
/// outlive that interval comfortably so a single failed pass does not blank
/// every badge in every wallet. Past this, an asset that has stopped being
/// measured — an RPC that lost its archive state, a venue that went away —
/// publishes no rate rather than one that is no longer being checked.
const MAX_ESTIMATE_AGE_MINUTES: i64 = 90;

/// One registered asset, as the catalog publishes it.
///
/// The table is written by protocol-indexer and read by every other service
/// through this crate, crossing a service boundary through the database.
#[derive(Debug, Clone, Queryable)]
pub struct AssetRow {
    pub asset_id_u64: i64,
    pub token: Vec<u8>,
    pub scale: BigDecimal,
    /// `NULL` until the indexer has read `decimals()` over RPC.
    pub decimals: Option<i16>,
    /// `NULL` until the indexer has read `symbol()`, or permanently for a token
    /// that does not implement it.
    pub symbol: Option<String>,
    /// Per-leg fee rates, `NULL` until an `AssetFeeSet` has been indexed.
    ///
    /// There is no pool-wide rate to fall back to, so `NULL` means unknown and
    /// a consumer must decline to quote rather than assume zero. Both are
    /// bounded by `MAX_FEE_BPS` (2000) on chain, so `SMALLINT` cannot hold a
    /// value a `uint16` could not.
    pub deposit_bps: Option<i16>,
    pub withdraw_bps: Option<i16>,
    /// The venue this asset's custody earns in, or `NULL` for a plain asset.
    ///
    /// Present iff the asset has an `asset_yield` row, which the contract
    /// creates once and can never undo.
    pub venue: Option<Vec<u8>>,
    /// Venue position plus idle, and the units outstanding against it.
    ///
    /// Both `NULL` until the indexer's first poll lands. A unit of this asset is
    /// worth `gross / supply` rather than `scale`, so a consumer that has the
    /// venue but not these two knows the asset is yield-bearing and that it
    /// cannot yet price it — which is different from pricing it at `scale` and
    /// being wrong by however much the venue has earned.
    pub gross: Option<BigDecimal>,
    pub total_normalized: Option<BigDecimal>,
    pub accrued_fee_normalized: Option<BigDecimal>,
    pub halted: Option<bool>,
    pub index_ray: Option<BigDecimal>,
    /// The pool's cut of the venue's yield, and the fraction of custody held
    /// idle for withdrawals. Both `NULL` until the asset has an `asset_yield`
    /// row; both bounded on chain, so `SMALLINT` cannot hold a value a `uint16`
    /// could not.
    ///
    /// Read only by the rate estimate, which reports what a note holder earns
    /// rather than what the venue paid — the difference between the two is
    /// exactly these.
    pub perf_bps: Option<i16>,
    pub buffer_bps: Option<i16>,
    /// The last measured rate, in basis points, net of `perf_bps` and
    /// `buffer_bps`. `NULL` until a measurement lands; see [`AssetRow::apy`],
    /// which is how it should be read.
    pub apy_bps: Option<i32>,
    /// Seconds spanned by the readings `apy_bps` came from.
    pub apy_window_s: Option<i64>,
    /// When the estimate was computed, on the measuring process's clock.
    /// `asset_yield.updated_at` is the indexer's heartbeat and would report a
    /// months-old rate as current.
    pub apy_measured_at: Option<DateTime<Utc>>,
    /// The `name()` of the ERC-4626 vault behind `venue`, as the vault itself
    /// reports it. `NULL` for a plain asset, until the indexer has read it, or
    /// permanently for a vault that does not implement it.
    pub vault_name: Option<String>,
}

impl AssetRow {
    /// The MASP asset id, stored as `i64` because Postgres has no unsigned
    /// integer. It is a `u64` everywhere else, and this is where the conversion
    /// belongs.
    pub fn asset_id(&self) -> u64 {
        self.asset_id_u64 as u64
    }

    /// This asset's circuit-to-base rate, or `None` if it cannot be priced yet.
    ///
    /// `None` means a yield asset whose index has not been polled — never a
    /// plain asset, which prices at `scale` forever. Callers must not substitute
    /// `scale` for a missing index: `scale` is not a conservative default but
    /// wrong by whatever the venue has earned, in the direction that quotes too
    /// many units and then credits too few.
    pub fn rate(&self, scale: Scale) -> Option<Rate> {
        if self.venue.is_none() {
            return Some(Rate::plain(scale));
        }
        let gross = bigdecimal_to_u256(self.gross.as_ref()?).ok()?;
        let total = bigdecimal_to_u256(self.total_normalized.as_ref()?).ok()?;
        let fee = bigdecimal_to_u256(self.accrued_fee_normalized.as_ref()?).ok()?;
        Some(Rate::yielding(scale, gross, total + fee))
    }

    /// The stored rate estimate, or `None` if there is none or it has gone
    /// stale.
    ///
    /// Read through here rather than off the three columns: an estimate that has
    /// stopped being refreshed must stop being published, and a caller reading
    /// `apy_bps` directly would serve it forever. All three columns are written
    /// together, so a partial row is a fault and yields nothing rather than half
    /// an answer.
    pub fn apy(&self) -> Option<ApyEstimate> {
        self.apy_at(Utc::now())
    }

    /// [`Self::apy`] against a caller-supplied clock, so the staleness rule is
    /// testable without waiting ninety minutes.
    pub fn apy_at(&self, now: DateTime<Utc>) -> Option<ApyEstimate> {
        let measured_at = self.apy_measured_at?;
        if now - measured_at > Duration::minutes(MAX_ESTIMATE_AGE_MINUTES) {
            return None;
        }
        Some(ApyEstimate {
            bps: self.apy_bps?,
            window_s: self.apy_window_s?,
        })
    }

    /// The venue address, or `None` for a plain asset or a column of the wrong
    /// width. Fallible for the same reason as [`Self::token_address`].
    pub fn venue_address(&self) -> Option<Address> {
        Address::try_from(self.venue.as_deref()?).ok()
    }

    /// The ERC-20 address, or `None` if the column does not hold 20 bytes.
    ///
    /// Fallible rather than `Address::from_slice`, which panics on any other
    /// width. The column is written by another service, so a row of the wrong shape
    /// is a data problem to report rather than one that stops the submit path.
    pub fn token_address(&self) -> Option<Address> {
        Address::try_from(self.token.as_slice()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A yield asset carrying a measurement taken `age_minutes` ago.
    fn measured(age_minutes: i64) -> AssetRow {
        AssetRow {
            asset_id_u64: 1,
            token: vec![0xAA; 20],
            scale: "1000000000000".parse().expect("scale"),
            decimals: Some(18),
            symbol: None,
            deposit_bps: None,
            withdraw_bps: None,
            venue: Some(vec![0xBB; 20]),
            gross: None,
            total_normalized: None,
            accrued_fee_normalized: None,
            halted: None,
            index_ray: None,
            perf_bps: None,
            buffer_bps: None,
            apy_bps: Some(512),
            apy_window_s: Some(7 * 24 * 60 * 60),
            apy_measured_at: Some(Utc::now() - Duration::minutes(age_minutes)),
            vault_name: None,
        }
    }

    #[test]
    fn a_fresh_estimate_is_published() {
        let est = measured(5).apy().expect("fresh estimate");
        assert_eq!(est.bps, 512);
        assert_eq!(est.window_s, 7 * 24 * 60 * 60);
    }

    /// The job the cache's TTL used to do: an asset that stops being measured
    /// goes back to publishing no rate rather than serving the last one forever.
    #[test]
    fn an_estimate_that_stopped_being_refreshed_is_dropped() {
        assert!(measured(MAX_ESTIMATE_AGE_MINUTES + 1).apy().is_none());
    }

    /// A single failed pass must not blank every badge, so the window is
    /// comfortably wider than the worker's 30-minute refresh.
    #[test]
    fn one_missed_refresh_does_not_drop_the_estimate() {
        assert!(measured(31).apy().is_some());
        assert!(measured(61).apy().is_some());
    }

    #[test]
    fn an_asset_never_measured_has_no_rate() {
        let mut row = measured(1);
        row.apy_bps = None;
        row.apy_window_s = None;
        row.apy_measured_at = None;
        assert!(row.apy().is_none());
    }

    /// The three columns are written together, so a row with only some of them
    /// is a fault. Report nothing rather than half an answer.
    #[test]
    fn a_partially_written_row_yields_nothing() {
        let mut missing_bps = measured(1);
        missing_bps.apy_bps = None;
        assert!(missing_bps.apy().is_none());

        let mut missing_window = measured(1);
        missing_window.apy_window_s = None;
        assert!(missing_window.apy().is_none());

        // No stamp means nothing can be aged, so it cannot be published at all.
        let mut missing_stamp = measured(1);
        missing_stamp.apy_measured_at = None;
        assert!(missing_stamp.apy().is_none());
    }

    /// A venue can lose, and the floor is a real rate rather than an error.
    #[test]
    fn a_negative_rate_is_published() {
        let mut loss = measured(1);
        loss.apy_bps = Some(-10_000);
        assert_eq!(loss.apy().expect("a loss is a rate").bps, -10_000);
    }
}
