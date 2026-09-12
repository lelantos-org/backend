//! Rate arithmetic: two readings and a span in, basis points out.
//!
//! Pure by construction — no chain, no database, no clock. Everything that
//! decides whether a pair of readings is allowed to become a published rate
//! lives here, so the rules are testable without either end of the measurement.
//! The measurement itself is [`crate::services::venue_apy`].

use alloy::primitives::U256;

/// One whole in basis points.
const BPS_DENOMINATOR: u32 = 10_000;

/// Seconds in a year, for the exponent. 365 days: a venue has no calendar, and a
/// fixed year keeps the figure reproducible from the two samples alone.
const YEAR_SECONDS: f64 = 365.0 * 24.0 * 60.0 * 60.0;

/// How far back the older sample is taken.
///
/// A week: long enough that one lumpy accrual does not become the rate, short
/// enough to describe the venue as it is now. Published alongside the figure —
/// a rate without its window is not a claim anyone can check.
pub const WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;

/// The shortest measured window worth annualizing.
///
/// The exponent is `year / window`, so a short window multiplies whatever it
/// caught — a single accrual, a rounding — by a large number. Below this the
/// samples are dropped rather than dressed up as a rate.
pub const MIN_WINDOW_SECONDS: i64 = 2 * 24 * 60 * 60;

/// The widest span the recorded history is allowed to answer over, and how long
/// a sample is kept.
///
/// Twice the window, so a gap in the record — a stopped relayer, a stalled
/// indexer — is survivable rather than blanking the figure, while a rate is
/// never measured over a quarter and called current.
pub const MAX_WINDOW_SECONDS: i64 = 2 * WINDOW_SECONDS;

/// Above this the readings are treated as garbage rather than as a rate: a
/// reindexed vault, a migration, or a window that straddled one. 10,000%.
const MAX_APY_BPS: i64 = 1_000_000;

/// The ratio of two readings, as a float.
///
/// Divided as integers first. Both readings are token amounts that can exceed
/// `u128`, and converting each to `f64` before dividing would round both
/// operands — the one rounding that would survive into the answer.
fn ratio(now: U256, then: U256) -> Option<f64> {
    const SCALE: u64 = 1_000_000_000_000;
    if then.is_zero() || now.is_zero() {
        return None;
    }
    let scaled = now.checked_mul(U256::from(SCALE))?.checked_div(then)?;
    let scaled = u128::try_from(scaled).ok()?;
    Some(scaled as f64 / SCALE as f64)
}

/// The annualized growth between two readings, in bps.
///
/// `None` whenever the pair cannot support a rate: too short a window, a zero
/// reading, or a result too large to be one. Applied to a vault's share price
/// this is gross of the pool's cut; see [`net_of_pool`]. Applied to two recorded
/// indices it is already net of everything.
pub fn annualize_bps(now: U256, then: U256, elapsed_s: i64) -> Option<i32> {
    if elapsed_s < MIN_WINDOW_SECONDS {
        return None;
    }
    let r = ratio(now, then)?;
    let apy = r.powf(YEAR_SECONDS / elapsed_s as f64) - 1.0;
    if !apy.is_finite() {
        return None;
    }
    let bps = (apy * f64::from(BPS_DENOMINATOR)).round() as i64;
    if bps > MAX_APY_BPS {
        return None;
    }
    // The floor is the only rate a total loss can produce, and it is a real one.
    Some(bps.max(-i64::from(BPS_DENOMINATOR)) as i32)
}

/// The vault's growth, less what the pool keeps of it.
///
/// `perf_bps` is skimmed off the yield; `buffer_bps` of custody is held idle for
/// withdrawals and earns nothing. Both shrink what reaches a note holder, and
/// both are bounded on chain, so neither can invert the sign here.
///
/// An approximation, deliberately: the buffer is a target the pool drifts around
/// rather than a constant, and a loss is not rebated by the performance fee. It
/// is applied to a loss all the same — reporting a loss as smaller than the
/// vault's would be the flattering direction, and the buffer genuinely damps
/// both.
pub fn net_of_pool(gross_bps: i32, perf_bps: i16, buffer_bps: i16) -> i32 {
    let whole = BPS_DENOMINATOR as i16;
    let keep = |bps: i16| 1.0 - (f64::from(bps.clamp(0, whole)) / f64::from(BPS_DENOMINATOR));
    (gross_bps as f64 * keep(perf_bps) * keep(buffer_bps)).round() as i32
}

/// The block the window starts at, estimated from a probe's block time.
///
/// An estimate only: the block it names then has its own timestamp read, and
/// that is what the rate is computed against. `None` when the probe says nothing
/// usable, or when the chain is not yet a window old.
pub fn window_start_block(
    head_number: u64,
    head_seconds: i64,
    probe_number: u64,
    probe_seconds: i64,
) -> Option<u64> {
    let blocks = head_number.checked_sub(probe_number)?;
    let seconds = head_seconds - probe_seconds;
    if blocks == 0 || seconds <= 0 {
        return None;
    }
    let per_block = seconds as f64 / blocks as f64;
    let back = (WINDOW_SECONDS as f64 / per_block).round();
    if !back.is_finite() || back <= 0.0 {
        return None;
    }
    let back = back as u64;
    if head_number <= back {
        return None;
    }
    Some(head_number - back)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A share price of `1.0 + pct/100`, in a vault whose probe reads `1e18`.
    fn price(pct: f64) -> U256 {
        U256::from((1e18 * (1.0 + pct / 100.0)) as u128)
    }

    const DAY: i64 = 24 * 60 * 60;

    #[test]
    fn compounds_a_window_up_to_a_year() {
        // 1% over a quarter is four such windows: 1.01^4 - 1 = 4.06%.
        let bps = annualize_bps(price(1.0), price(0.0), 365 * DAY / 4).unwrap();
        assert_eq!(bps, 406);
    }

    #[test]
    fn is_the_growth_itself_over_exactly_a_year() {
        let bps = annualize_bps(price(5.0), price(0.0), 365 * DAY).unwrap();
        assert_eq!(bps, 500);
    }

    /// The exponent is `year / window`, so a short window multiplies whatever it
    /// caught. An hour of drift is not a rate and is not reported as one.
    #[test]
    fn refuses_a_window_under_the_floor() {
        assert!(annualize_bps(price(0.02), price(0.0), MIN_WINDOW_SECONDS - 1).is_none());
        assert!(annualize_bps(price(0.02), price(0.0), MIN_WINDOW_SECONDS).is_some());
    }

    #[test]
    fn reports_a_venue_loss_rather_than_clamping_it() {
        let bps = annualize_bps(price(0.0), price(5.0), 365 * DAY).unwrap();
        assert!(bps < 0, "{bps}");
    }

    /// A vault reindexed inside the window leaves two readings with no common
    /// basis. The ratio is arithmetic; the rate it implies is fiction.
    #[test]
    fn drops_a_reading_too_wild_to_be_a_rate() {
        let wild = U256::from(10_000u64) * price(0.0);
        assert!(annualize_bps(wild, price(0.0), 30 * DAY).is_none());
    }

    #[test]
    fn has_no_answer_without_both_readings() {
        assert!(annualize_bps(price(1.0), U256::ZERO, 30 * DAY).is_none());
        assert!(annualize_bps(U256::ZERO, price(1.0), 30 * DAY).is_none());
    }

    /// Both readings can exceed `u128`, so the division has to happen before
    /// anything becomes a float.
    #[test]
    fn keeps_precision_on_large_readings() {
        let then = U256::from(10u64).pow(U256::from(40u64));
        let now = then * U256::from(101u64) / U256::from(100u64);
        assert_eq!(annualize_bps(now, then, 365 * DAY).unwrap(), 100);
    }

    #[test]
    fn nets_out_what_the_pool_keeps() {
        // 10% of the yield to the treasury, a fifth of custody held idle.
        assert_eq!(net_of_pool(500, 1_000, 2_000), 360);
        // Nothing kept, nothing idle: the vault's rate reaches the holder whole.
        assert_eq!(net_of_pool(500, 0, 0), 500);
        // A loss is damped by the buffer too — the idle fraction did not lose
        // either. Reporting the vault's full loss would be the wrong direction.
        assert_eq!(net_of_pool(-500, 0, 2_000), -400);
    }

    #[test]
    fn converts_the_window_into_blocks_at_the_measured_block_time() {
        // 2s blocks: a week is 302,400 of them.
        assert_eq!(
            window_start_block(1_000_000, 10_000, 995_000, 0),
            Some(697_600)
        );
        // 12s blocks: a sixth as many.
        assert_eq!(
            window_start_block(1_000_000, 60_000, 995_000, 0),
            Some(949_600)
        );
    }

    #[test]
    fn has_no_window_on_a_chain_younger_than_one() {
        // 12s blocks, 1,000 blocks of history: a week ago is before genesis.
        assert_eq!(window_start_block(1_000, 12_000, 0, 0), None);
    }

    /// Both are shapes a node can return: a reorg-adjacent head, or timestamps
    /// that did not advance between two blocks.
    #[test]
    fn has_no_window_from_a_probe_that_says_nothing() {
        assert_eq!(window_start_block(1_000_000, 0, 995_000, 0), None);
        assert_eq!(window_start_block(100, 1_000, 100, 1_000), None);
    }
}
