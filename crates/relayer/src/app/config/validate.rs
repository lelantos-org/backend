//! Boot-time checks on values the rest of the code assumes are sound.

use super::{BPS_DENOMINATOR, ChainCfg, MAX_BUNDLE_ITEMS, RelayerConfig};

/// Smallest `max_tx_bytes` that still fits one swap, the largest operation.
const MIN_MAX_TX_BYTES: usize = 16_000;

/// The widest exponent `10u128.pow(n)` accepts.
const MAX_DECIMALS: u8 = 38;

/// A 100x markup. Anything above this is a typo, and `10_000 + bps` must not
/// overflow.
const MAX_MARKUP_BPS: u32 = 1_000_000;

/// Everything wrong with a config, rendered as one message.
#[derive(Debug)]
pub struct ConfigErrors(Vec<String>);

impl std::fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} problem(s):", self.0.len())?;
        for p in &self.0 {
            write!(f, "\n  - {p}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigErrors {}

impl ChainCfg {
    /// Everything wrong with this chain's settings.
    fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut check = |ok: bool, msg: String| {
            if !ok {
                out.push(format!("chain {}: {msg}", self.chain_id));
            }
        };
        check(
            self.native_decimals <= MAX_DECIMALS,
            format!(
                "native_decimals {} exceeds {MAX_DECIMALS}",
                self.native_decimals
            ),
        );
        for t in &self.accepted_fee_tokens {
            check(
                t.decimals <= MAX_DECIMALS,
                format!(
                    "fee token {} decimals {} exceeds {MAX_DECIMALS}",
                    t.symbol, t.decimals
                ),
            );
        }
        check(
            self.fee_markup_bps <= MAX_MARKUP_BPS,
            format!(
                "fee_markup_bps {} exceeds {MAX_MARKUP_BPS}",
                self.fee_markup_bps
            ),
        );
        check(
            self.flush_interval_s > 0,
            "flush_interval_s must be > 0".to_string(),
        );
        check(
            (1..=MAX_BUNDLE_ITEMS).contains(&self.bundle_max_items),
            format!(
                "bundle_max_items {} must be between 1 and {MAX_BUNDLE_ITEMS}",
                self.bundle_max_items
            ),
        );
        check(
            self.max_tx_bytes >= MIN_MAX_TX_BYTES,
            format!(
                "max_tx_bytes {} is below {MIN_MAX_TX_BYTES}, which does not fit a single swap",
                self.max_tx_bytes
            ),
        );
        check(
            self.shielded_fee_grace_bps < BPS_DENOMINATOR,
            format!(
                "shielded_fee_grace_bps {} must be below {BPS_DENOMINATOR}; at or above it every \
                 fee, including none at all, clears the check",
                self.shielded_fee_grace_bps
            ),
        );
        // An address without the key rejects every spend, and a key without an
        // address collects nothing. Neither is visible at runtime, so both are
        // fatal here.
        check(
            self.shielded_fee_address.is_some() == self.shielded_fee_ivk.is_some(),
            "shielded_fee_address and shielded_fee_ivk must be set together".to_string(),
        );
        check(
            self.shielded_fee_address.is_some() || self.shielded_fee_assets.is_empty(),
            "shielded_fee_assets is set but shielded_fee_address is not, so no fee is collected"
                .to_string(),
        );
        out
    }
}

impl RelayerConfig {
    /// Boot-time checks on values the rest of the code assumes are sound.
    ///
    /// Each would otherwise fail at runtime far from its cause: a duplicate chain
    /// runs two independent tree mirrors against one chain, and the decimal and
    /// markup bounds guard arithmetic that panics rather than erroring.
    ///
    /// Every problem is reported rather than only the first, so an operator fixing
    /// a config does not restart once per mistake.
    pub fn validate(&self) -> Result<(), ConfigErrors> {
        let mut problems = Vec::new();
        if self.chains.is_empty() {
            problems.push("no chains configured".to_string());
        }
        let mut seen = std::collections::HashSet::new();
        for c in &self.chains {
            if !seen.insert(c.chain_id) {
                problems.push(format!(
                    "chain_id {} is declared more than once; each chain owns one tree mirror \
                     and one flush worker, so a duplicate desyncs both",
                    c.chain_id
                ));
            }
            problems.extend(c.problems());
            // A dry run cannot catch an invalid wallet proof, since it stubs the
            // verifiers out; without local verification one bad proof is found
            // only by the real simulation, after every other item was proved.
            if c.bundle_max_items > 1 && self.prover.transact_vkey_path.is_none() {
                problems.push(format!(
                    "chain {}: bundle_max_items {} requires prover.transact_vkey_path",
                    c.chain_id, c.bundle_max_items
                ));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(ConfigErrors(problems))
        }
    }
}
