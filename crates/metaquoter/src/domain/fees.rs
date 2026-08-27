use alloy::primitives::U256;

/// Wrapper overhead added on top of the venue's reported `gasEstimate`: two
/// MASP transacts (~250k each, inline verifier) plus ~85k wrapper bookkeeping.
pub const WRAPPER_OVERHEAD_GAS: u64 = 585_000;

/// Largest `expected_out` such that
/// `expected_out + expected_out * bps / 10_000 <= gross`, equivalent to
/// `gross * 10_000 / (10_000 + bps)`. `bps` is clamped to 10_000.
///
/// MASP charges its fee on top of the deposited amount (`MASP._computeAmounts`),
/// so the depositable figure is the reciprocal of the fee applied to gross
/// rather than gross minus fee.
pub fn max_deposit(gross: U256, bps: u16) -> U256 {
    let bps = u32::from(bps).min(10_000);
    let denom = U256::from(10_000u32 + bps);
    gross.saturating_mul(U256::from(10_000u32)) / denom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_deposit_reciprocal_fee() {
        assert_eq!(
            max_deposit(U256::from(1_000_000u64), 0),
            U256::from(1_000_000u64)
        );

        // 50 bps fee: expected_out = gross * 10_000 / 10_050 ≈ 995_024, so
        // masp_fee = gross - expected_out ≈ 4_975 ≈ expected_out * 50/10_000.
        let gross = U256::from(1_000_000u64);
        let expected = max_deposit(gross, 50);
        let fee = gross - expected;
        assert_eq!(expected, U256::from(995_024u64));
        // Fee on expected_out, rounded down to match integer arithmetic.
        assert_eq!(
            fee,
            expected * U256::from(50u32) / U256::from(10_000u32) + U256::from(1u8)
        );

        assert_eq!(max_deposit(gross, u16::MAX), max_deposit(gross, 10_000));
    }
}
