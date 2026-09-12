//! The best-known chain head, per chain.
//!
//! Updated opportunistically from responses that happen to carry it, rather
//! than by a background poller. A poller would add a fixed idle cost — roughly
//! 39M compute units a month across three chains, spent while nobody is using
//! the app — and buy nothing on the hot path, because the one-second cache on
//! `eth_blockNumber` already caps the upstream at about one request per second
//! per chain.
//!
//! An unknown head is safe by construction: [`crate::domain::policy`] treats
//! every read as if it were at the tip, so the cost of not knowing is a shorter
//! TTL, never a stale answer.

use crate::domain::blocktag::parse_quantity;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Shared, lock-free head tracker.
#[derive(Debug, Clone, Default)]
pub struct Tip(Arc<AtomicU64>);

/// Sentinel for "no head observed yet". Zero is not a plausible head for any
/// chain this serves, and using it avoids an `Option` behind an atomic.
const UNKNOWN: u64 = 0;

impl Tip {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> Option<u64> {
        match self.0.load(Ordering::Relaxed) {
            UNKNOWN => None,
            n => Some(n),
        }
    }

    /// Record an observed height.
    ///
    /// Monotonic: a lower value is ignored. Responses can arrive out of order,
    /// and letting the head go backwards would flip finalized reads back to the
    /// short TTL for no reason.
    pub fn observe(&self, height: u64) {
        if height == UNKNOWN {
            return;
        }
        self.0.fetch_max(height, Ordering::Relaxed);
    }

    /// Read a head out of an `eth_blockNumber` result.
    pub fn observe_block_number(&self, raw: &RawValue) {
        if let Some(n) = quantity(raw) {
            self.observe(n);
        }
    }

    /// Read a head out of a block body's `number`, and return it.
    ///
    /// A block names its own height `number`; `blockNumber` appears only on
    /// the transactions nested inside it, which are not read.
    pub fn observe_block(&self, raw: &RawValue) -> Option<u64> {
        self.observe_field(raw, |h| h.number)
    }

    /// Read a head out of a receipt or transaction body's `blockNumber`, and
    /// return it.
    pub fn observe_inclusion(&self, raw: &RawValue) -> Option<u64> {
        self.observe_field(raw, |h| h.block_number)
    }

    /// Record the height `pick` chooses from a body, and return it.
    ///
    /// The height is returned rather than only recorded because the caller needs
    /// the same field to choose a cache class for a result-classified method.
    /// A receipt carries every log the transaction emitted and a full block
    /// every transaction, so parsing either twice to read one field is the most
    /// expensive avoidable thing on these paths — and receipts are polled once a
    /// second per pending deposit.
    fn observe_field<'a>(
        &self,
        raw: &'a RawValue,
        pick: fn(Heights<'a>) -> Option<&'a str>,
    ) -> Option<u64> {
        let heights: Heights<'a> = serde_json::from_str(raw.get()).ok()?;
        let n = pick(heights).and_then(parse_quantity)?;
        self.observe(n);
        Some(n)
    }
}

/// The two top-level height fields a response body can carry.
///
/// Deserialized in place of a `serde_json::Value`: serde skips every other
/// field — a receipt's logs, a block's transactions — without allocating for
/// it, and the heights borrow from the body. A hex quantity never contains a
/// JSON escape, so borrowing cannot fail on a well-formed one.
#[derive(Deserialize)]
struct Heights<'a> {
    number: Option<&'a str>,
    #[serde(rename = "blockNumber")]
    block_number: Option<&'a str>,
}

/// A JSON string holding a hex quantity.
fn quantity(raw: &RawValue) -> Option<u64> {
    let s: String = serde_json::from_str(raw.get()).ok()?;
    parse_quantity(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(s: &str) -> Box<RawValue> {
        RawValue::from_string(s.to_string()).unwrap()
    }

    #[test]
    fn an_unobserved_tip_is_unknown() {
        assert_eq!(Tip::new().get(), None);
    }

    #[test]
    fn a_block_number_response_sets_the_tip() {
        let t = Tip::new();
        t.observe_block_number(&raw(r#""0xf4240""#));
        assert_eq!(t.get(), Some(1_000_000));
    }

    /// The height is both recorded and handed back, so the caller need not
    /// parse the body a second time to classify it.
    #[test]
    fn a_receipt_body_sets_the_tip_and_reports_its_height() {
        let t = Tip::new();
        let h = t.observe_inclusion(&raw(
            r#"{"blockNumber":"0xf4240","logs":[{"data":"0x00","topics":["0xaa"]}]}"#,
        ));
        assert_eq!(h, Some(1_000_000));
        assert_eq!(t.get(), Some(1_000_000));
    }

    /// A block's own height is `number`. The `blockNumber` on its nested
    /// transactions must not be mistaken for it.
    #[test]
    fn a_block_body_is_read_by_its_number() {
        let t = Tip::new();
        let block =
            raw(r#"{"number":"0xf4240","hash":"0xab","transactions":[{"blockNumber":"0x1"}]}"#);
        assert_eq!(t.observe_block(&block), Some(1_000_000));
        assert_eq!(t.get(), Some(1_000_000));
        assert_eq!(
            Tip::new().observe_inclusion(&block),
            None,
            "a block has no top-level blockNumber"
        );
    }

    /// A body with no usable height reports none, which is how an unmined
    /// receipt stays uncached.
    #[test]
    fn a_body_without_a_height_reports_none() {
        let t = Tip::new();
        assert_eq!(t.observe_inclusion(&raw("null")), None);
        assert_eq!(t.observe_inclusion(&raw(r#"{"blockNumber":null}"#)), None);
        assert_eq!(t.observe_block(&raw("null")), None);
        assert_eq!(t.observe_block(&raw(r#"{"number":null}"#)), None);
    }

    /// Responses can arrive out of order. A head that went backwards would flip
    /// finalized reads back to the two-second TTL for no reason.
    #[test]
    fn the_tip_never_goes_backwards() {
        let t = Tip::new();
        t.observe(1_000);
        t.observe(900);
        assert_eq!(t.get(), Some(1_000));
    }

    /// A malformed or null result must leave the head alone rather than
    /// clearing it.
    #[test]
    fn a_malformed_result_is_ignored() {
        let t = Tip::new();
        t.observe(1_000);
        t.observe_block_number(&raw("null"));
        t.observe_block_number(&raw(r#""not-hex""#));
        t.observe_inclusion(&raw("null"));
        t.observe_inclusion(&raw(r#"{"blockNumber":null}"#));
        t.observe_block(&raw(r#"{"number":7}"#));
        assert_eq!(t.get(), Some(1_000));
    }
}
