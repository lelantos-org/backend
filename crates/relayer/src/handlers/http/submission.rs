//! The shared body of `/v1/spend` and `/v1/swap`.
//!
//! Both endpoints spend shielded notes and differ only in which pipeline executes
//! the payload. Admission control, the replay window and the fingerprint pinning
//! a key to its payload are identical, and the order of those steps matters, so
//! they live here once rather than being restated per endpoint.

use crate::adapters::parse::{FieldRef, parse_field};
use crate::app::AppState;
use crate::domain::dto::{
    PubInputsDto, SubmitSpendPayload, SubmitSwapPayload, TRANSACT_IN, TRANSACT_OUT,
};
use crate::domain::error::AppResult;
use crate::domain::responses::RelayerSubmitResponse;
use crate::services::nullifier_guard::{Nullifiers, nullifiers_of};
use crate::services::submitter::SubmissionReceipt;
use alloy::primitives::keccak256;
use axum::Json;
use axum::http::HeaderMap;
use std::future::Future;

/// A request that spends shielded notes.
///
/// Implemented by both submission payloads so [`submit`] can drive either
/// without knowing which it holds.
pub trait Submission {
    fn chain_id(&self) -> i64;
    fn pub_inputs(&self) -> &PubInputsDto;
}

impl Submission for SubmitSpendPayload {
    fn chain_id(&self) -> i64 {
        self.chain_id
    }

    fn pub_inputs(&self) -> &PubInputsDto {
        &self.pub_inputs
    }
}

impl Submission for SubmitSwapPayload {
    fn chain_id(&self) -> i64 {
        self.chain_id
    }

    fn pub_inputs(&self) -> &PubInputsDto {
        &self.pub_inputs
    }
}

/// Run one note-spending submission end to end.
///
/// The ordering matters:
///
/// 1. Nullifiers and the fingerprint are parsed before the idempotency run, so a
///    malformed payload is a 400 rather than a cached answer under a key.
/// 2. The nullifier reservation happens inside that run, because a resubmission
///    under a known key must replay the first answer; reserving outside would
///    refuse it as a double-spend first.
/// 3. The reservation is marked spent only after the transaction lands, holding
///    it against a resubmit until the indexer catches up.
pub async fn submit<P, Run, Fut>(
    st: AppState,
    headers: HeaderMap,
    payload: P,
    run: Run,
) -> AppResult<Json<RelayerSubmitResponse>>
where
    P: Submission,
    Run: FnOnce(P) -> Fut,
    Fut: Future<Output = AppResult<SubmissionReceipt>>,
{
    let chain_id = payload.chain_id();
    let nfs = nullifiers_of(payload.pub_inputs())?;
    let fingerprint = submission_fingerprint(&nfs, payload.pub_inputs())?;

    let tx_hash = st
        .idempotency
        .clone()
        .run(chain_id, idempotency_key(&headers), fingerprint, async {
            let reservation = st.nullifiers.reserve(&st.pool, chain_id, nfs).await?;
            // Boxed rather than inlined: a pipeline future is hundreds of
            // kilobytes, and every in-flight request would otherwise carry the
            // whole state machine on the handler's frame.
            let receipt = Box::pin(run(payload)).await?;
            reservation.spent().await;
            Ok(format!("0x{}", hex::encode(receipt.tx_hash)))
        })
        .await?;

    Ok(Json(RelayerSubmitResponse { tx_hash }))
}

/// Header a client sends to make a resubmission replayable rather than a
/// second spend. One key per submission, held across the client's own retries.
const IDEMPOTENCY_KEY: &str = "Idempotency-Key";

/// Longest key accepted. Keys are held in memory for their TTL, so the length a
/// caller may pin is bounded; the SDK's is 32 hex characters.
const MAX_KEY_LEN: usize = 128;

/// The caller's idempotency key, if it sent a usable one.
///
/// A malformed or oversized header is ignored rather than rejected: it costs the
/// caller replay protection only, while failing the submission would turn a
/// header problem into a failed spend.
pub fn idempotency_key(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(IDEMPOTENCY_KEY)?.to_str().ok()?.trim();
    (!raw.is_empty() && raw.len() <= MAX_KEY_LEN).then(|| raw.to_string())
}

/// What identifies this submission.
///
/// The idempotency key is chosen by the caller and carries no proof of what it
/// stands for. Storing this alongside the answer lets a key reused with different
/// contents be refused, rather than accepting the second submission and returning
/// the first one's transaction hash.
///
/// Nullifiers and output commitments suffice: two payloads agreeing on both spend
/// the same notes into the same notes.
///
/// Both are hashed as their parsed 32-byte values rather than as wire strings.
/// The encoding is caller-chosen — `"1"` and `"0x01"` are the same commitment —
/// so hashing the text would make a re-encoded retry look like a different
/// submission and defeat the replay protection.
pub fn submission_fingerprint(nfs: &Nullifiers, pi: &PubInputsDto) -> AppResult<[u8; 32]> {
    let mut preimage = [[0u8; 32]; TRANSACT_IN + TRANSACT_OUT];
    preimage[..TRANSACT_IN].copy_from_slice(nfs);
    for (i, cm) in pi.out_cm.iter().enumerate() {
        preimage[TRANSACT_IN + i] = parse_field(cm, FieldRef::Index("pubInputs.outCm", i))?.0;
    }
    Ok(keccak256(preimage.concat()).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(IDEMPOTENCY_KEY, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn reads_the_key_a_client_sent() {
        assert_eq!(
            idempotency_key(&headers("  abc123  ")).as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn treats_an_unusable_key_as_no_key() {
        assert_eq!(idempotency_key(&HeaderMap::new()), None);
        assert_eq!(idempotency_key(&headers("   ")), None);
        assert_eq!(
            idempotency_key(&headers(&"k".repeat(MAX_KEY_LEN + 1))),
            None
        );
    }
}
