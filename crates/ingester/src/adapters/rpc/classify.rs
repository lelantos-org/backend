//! Provider error strings → the taxonomy the callers act on.
//!
//! Providers disagree on both the code and the wording for the same refusal, so
//! the classification is substring matching over the message, kept apart from
//! the transport it describes: it is pure, and it is the piece with a
//! regression history worth testing on its own.

use crate::domain::error::RpcError;

/// Substrings that mean "your query asked for too much".
///
/// An unrecognised limit error skips the halving path and fails the fetch
/// instead of narrowing it. Kept lowercase and matched against a lowercased
/// message.
///
/// Deliberately carries no bare error code. `-32005` is Infura's, and Infura
/// spends it on rate limits as often as on oversized queries, so matching the
/// code rather than the wording classified every throttle as a range cap. The
/// wording alone is unambiguous.
///
/// Each entry is a substring no other entry already contains, so
/// "range is too large" also covers "block range is too large", and
/// "response size" covers "log response size exceeded".
const RANGE_MARKERS: &[&str] = &[
    "query returned more than",
    "block range too large",
    "exceed maximum block range",
    "range is too large",
    "too many results",
    "response size",
    "query timeout exceeded",
];

/// Substrings that mean "you are asking too often".
///
/// Checked *before* [`RANGE_MARKERS`], because the two vocabularies overlap and
/// the two mistakes are not symmetric. Reading a throttle as a range cap makes
/// [`crate::services::log_range::fetch_adaptive`] halve its window and publish
/// the result as a learned ceiling, so a burst of 429s pins every later query on
/// this provider to a fraction of what it actually serves. Reading a range cap
/// as a throttle only costs a retry.
///
/// `"limit exceeded"` is the specific string that made this ordering necessary:
/// it is a substring of "rate limit exceeded", which is what most providers send
/// when throttling.
const RATE_LIMIT_MARKERS: &[&str] = &[
    "429",
    "too many requests",
    "rate limit",
    // Infura, for a project exceeding its per-second or daily allowance.
    "rate exceeded",
    "request count exceeded",
    // Alchemy, for compute units per second.
    "capacity",
    "limit exceeded",
];

/// `-32602` is a generic "invalid params". It is read as a range problem only
/// when the message also mentions the range or the result set; otherwise a
/// malformed filter would be halved indefinitely instead of surfacing.
fn is_oversized_params(msg: &str) -> bool {
    msg.contains("-32602") && ["range", "results", "logs"].iter().any(|w| msg.contains(w))
}

/// Map a provider error onto the taxonomy the callers act on.
pub(super) fn classify<E: std::fmt::Display>(err: E) -> RpcError {
    let raw = err.to_string();
    let msg = raw.to_lowercase();

    // Rate limits first; see [`RATE_LIMIT_MARKERS`] for why the order is not
    // arbitrary.
    if RATE_LIMIT_MARKERS.iter().any(|m| msg.contains(m)) {
        RpcError::RateLimited
    } else if RANGE_MARKERS.iter().any(|m| msg.contains(m)) || is_oversized_params(&msg) {
        RpcError::RangeTooLarge
    } else {
        RpcError::Other(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Error strings observed from the major providers. A miss on any of these
    /// means the adaptive fetch never halves and the backfill fails instead of
    /// narrowing its window.
    #[test]
    fn classifies_provider_range_errors() {
        let range = [
            "server returned an error response: error code -32005: query returned more than 10000 results",
            "error code -32602: Log response size exceeded. You can make eth_getLogs requests with up to a 2K block range",
            "query returned more than 10000 results",
            "block range is too large, max is 1000",
            "Query timeout exceeded. Consider reducing your block range",
        ];
        for e in range {
            assert!(
                matches!(classify(e), RpcError::RangeTooLarge),
                "expected RangeTooLarge for {e:?}, got {:?}",
                classify(e)
            );
        }
    }

    #[test]
    fn classifies_rate_limits() {
        for e in [
            "HTTP status 429 Too Many Requests",
            "your app has exceeded its rate limit",
            "error code -32005: project ID request rate exceeded",
            "error code -32005: daily request count exceeded, request rate limited",
            "Your app has exceeded its compute units per second capacity",
        ] {
            assert!(
                matches!(classify(e), RpcError::RateLimited),
                "expected RateLimited for {e:?}, got {:?}",
                classify(e)
            );
        }
    }

    /// The regression this ordering exists for. "limit exceeded" is a substring
    /// of the message most providers send when throttling, and it used to sit in
    /// `RANGE_MARKERS`, which is checked against the same string. Classifying a
    /// throttle as a range cap makes the adaptive fetcher halve its window and
    /// publish the result as a learned ceiling, which never rises again — so one
    /// burst of 429s left the provider being queried one block at a time for the
    /// life of the process.
    #[test]
    fn throttling_is_never_read_as_a_range_cap() {
        for e in [
            "rate limit exceeded",
            "request rate limit exceeded",
            "error code -32005: limit exceeded",
            "daily limit exceeded",
        ] {
            assert!(
                matches!(classify(e), RpcError::RateLimited),
                "expected RateLimited for {e:?}, got {:?}",
                classify(e)
            );
        }
    }

    /// A bare `-32602` is a generic "invalid params" and must not be read as a
    /// range problem, or the fetcher halves indefinitely over a malformed filter.
    #[test]
    fn leaves_unrelated_errors_alone() {
        for e in [
            "error code -32602: invalid argument 0: hex string has odd length",
            "connection reset by peer",
        ] {
            assert!(
                matches!(classify(e), RpcError::Other(_)),
                "expected Other for {e:?}, got {:?}",
                classify(e)
            );
        }
    }
}
