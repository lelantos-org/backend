//! Turning a venue lens's `eth_call` outcomes into a quote or an error.
//!
//! Shared by both venue adapters: they differ in the ABI and in what a "tier"
//! is, not in how a failed call should be read.

use crate::domain::error::AppError;
use crate::domain::models::Venue;
use alloy::contract::Error as ContractError;
use alloy::primitives::U256;
use alloy::transports::RpcError;
use std::future::Future;
use tracing::debug;

/// The marker a node puts in a JSON-RPC error when the call reverted.
///
/// Matched on text because no code distinguishes it: geth answers `-32000` and
/// other nodes `3`, both of which also cover errors that are not reverts.
const REVERT_MARKER: &str = "execution reverted";

/// Why one tier produced no quote.
enum TierFailure {
    /// The lens reverted. Both `QuoterV2` and `V4Quoter` revert for a pool that
    /// is not deployed or not initialized, which is the ordinary case for three
    /// of the four tiers in a fan-out, so it is dropped rather than surfaced.
    NoPool,
    /// The call reached no verdict: a refused connection, a rate limit, a
    /// gateway 5xx, a response that would not decode against the ABI. Carries
    /// the driver string for the log.
    ///
    /// Kept distinct from [`TierFailure::NoPool`] because it is not evidence
    /// about liquidity. Folding the two lets one unreachable node answer "this
    /// pair has no pool", which is a claim about the chain that nothing
    /// supports.
    Unresolved(String),
}

/// Read a failed venue call.
///
/// Only an `ErrorResp` naming a revert is a missing pool. Every other
/// `ErrorResp` — `-32005 limit exceeded`, `-32601 method not found`, an
/// execution timeout — is a node refusing to answer, and an `AbiError` means the
/// configured quoter address is answering but is not the contract it was
/// configured as. None of those is liquidity information.
fn classify(e: ContractError) -> TierFailure {
    match &e {
        ContractError::TransportError(RpcError::ErrorResp(payload))
            if payload.message.contains(REVERT_MARKER) =>
        {
            TierFailure::NoPool
        }
        _ => TierFailure::Unresolved(e.to_string()),
    }
}

/// Run every tier call concurrently and keep the highest-ranked result.
///
/// Reverting tiers are dropped; one tier without a pool must not fail a request
/// another tier can serve. Unresolved tiers are not dropped, and if nothing came
/// back while at least one tier never got an answer the result is
/// [`AppError::Rpc`] rather than [`AppError::NoLiquidity`] — the difference the
/// caller acts on, since one is worth retrying and the other is not.
///
/// A single revert alongside successes stays invisible: a pair really can have
/// three empty tiers and one deep one.
pub async fn best_tier<T, Fut, K, B>(
    venue: Venue,
    calls: impl IntoIterator<Item = Fut>,
    key: K,
) -> Result<T, AppError>
where
    Fut: Future<Output = Result<T, ContractError>>,
    K: FnMut(&T) -> B,
    B: Ord,
{
    let mut quoted = Vec::new();
    // The first unresolved tier, kept only to describe the failure if no tier
    // succeeds. Later ones add nothing: they are almost always the same node
    // failing the same way four times over.
    let mut unresolved: Option<String> = None;

    for outcome in futures::future::join_all(calls).await {
        match outcome {
            Ok(t) => quoted.push(t),
            Err(e) => match classify(e) {
                TierFailure::NoPool => {}
                TierFailure::Unresolved(detail) => {
                    debug!(?venue, error = %detail, "tier call reached no verdict");
                    unresolved.get_or_insert(detail);
                }
            },
        }
    }

    match quoted.into_iter().max_by_key(key) {
        Some(best) => Ok(best),
        None => Err(unresolved.map_or(AppError::NoLiquidity, AppError::Rpc)),
    }
}

/// A venue's reported `gasEstimate` narrowed to the `u64` a [`Quote`] carries.
///
/// Both lenses return it as a `uint256`. A figure that does not fit is not a
/// real gas estimate, and dropping an otherwise good tier over it would lose a
/// quote, so it saturates rather than erroring.
///
/// [`Quote`]: crate::domain::models::Quote
pub fn gas_estimate(reported: U256) -> u64 {
    reported.try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::rpc::json_rpc::ErrorPayload;
    use std::pin::Pin;

    /// No two async blocks share a type, so the calls in one fan-out have to be
    /// boxed to sit in the same collection. The real fan-outs come from a single
    /// `map` closure and are one type already.
    type TierCall = Pin<Box<dyn Future<Output = Result<u64, ContractError>>>>;

    fn quoted(amount_out: u64) -> TierCall {
        Box::pin(async move { Ok(amount_out) })
    }

    fn failed(e: ContractError) -> TierCall {
        Box::pin(async move { Err(e) })
    }

    fn error_resp(code: i64, message: &str) -> ContractError {
        ContractError::TransportError(RpcError::ErrorResp(ErrorPayload {
            code,
            message: message.to_string().into(),
            data: None,
        }))
    }

    async fn best(calls: Vec<TierCall>) -> Result<u64, AppError> {
        best_tier(Venue::UniV3, calls, |v: &u64| *v).await
    }

    /// The ordinary case: three of four canonical tiers have no pool for a given
    /// pair, and the request must still be served by the fourth.
    #[tokio::test]
    async fn reverting_tiers_are_dropped_and_the_best_survivor_wins() {
        let got = best(vec![
            failed(error_resp(3, "execution reverted")),
            quoted(10),
            quoted(30),
            failed(error_resp(-32000, "execution reverted")),
        ])
        .await
        .unwrap();
        assert_eq!(got, 30);
    }

    /// Every tier reverting is the one case that really is a missing pair.
    #[tokio::test]
    async fn every_tier_reverting_is_no_liquidity() {
        let err = best(vec![
            failed(error_resp(3, "execution reverted")),
            failed(error_resp(3, "execution reverted: STF")),
        ])
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::NoLiquidity), "{err}");
    }

    /// A rate limit is an `ErrorResp` like a revert is, and reading it as one
    /// would report a 422 "no liquidity for pair" for a pool the node simply
    /// declined to look at.
    #[tokio::test]
    async fn a_node_that_refuses_to_answer_is_not_no_liquidity() {
        let err = best(vec![
            failed(error_resp(-32005, "limit exceeded")),
            failed(error_resp(3, "execution reverted")),
        ])
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Rpc(_)), "{err}");
    }

    /// A quoter address pointing at something that is not a quoter answers, and
    /// the answer will not decode. That is a deployment fault, not an empty
    /// pool, and must not be reported as one.
    #[tokio::test]
    async fn an_undecodable_answer_is_not_no_liquidity() {
        let err = best(vec![failed(ContractError::from(
            alloy::sol_types::Error::Overrun,
        ))])
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Rpc(_)), "{err}");
    }

    /// An unresolved tier alongside a successful one is not an error: the quote
    /// stands on the tier that answered.
    #[tokio::test]
    async fn one_unresolved_tier_does_not_fail_a_served_request() {
        let got = best(vec![
            failed(error_resp(-32005, "limit exceeded")),
            quoted(7),
        ])
        .await
        .unwrap();
        assert_eq!(got, 7);
    }
}
