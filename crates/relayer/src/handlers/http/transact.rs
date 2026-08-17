use crate::adapters::parse::parse_b32;
use crate::app::AppState;
use crate::domain::dto::SubmitSpendPayload;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::RelayerSubmitResponse;
use crate::services::pipeline::spend::SpendPipeline;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use std::sync::Arc;
use tracing::instrument;

/// Header a client sends to make a resubmission replayable rather than a
/// second spend. One key per submission, held across the client's own retries.
const IDEMPOTENCY_KEY: &str = "Idempotency-Key";

/// Longest key accepted. Keys are held in memory for their TTL, so the length
/// a caller may pin is bounded; the SDK's is 32 hex characters.
const MAX_KEY_LEN: usize = 128;

/// The caller's idempotency key, if it sent a usable one.
///
/// A malformed or oversized header is ignored rather than rejected: it only
/// costs the caller replay protection, and failing the submission over it
/// would turn a header problem into a failed spend.
fn idempotency_key(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(IDEMPOTENCY_KEY)?.to_str().ok()?.trim();
    (!raw.is_empty() && raw.len() <= MAX_KEY_LEN).then(|| raw.to_string())
}

#[instrument(skip_all, fields(chain_id = payload.chain_id, kind = ?payload.kind))]
pub async fn submit_spend(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<SubmitSpendPayload>,
) -> AppResult<Json<RelayerSubmitResponse>> {
    let chain_id = payload.chain_id;
    let pipeline = st
        .spend_pipelines
        .get(&chain_id)
        .ok_or(AppError::UnknownChain(chain_id))?
        .clone();

    // Everything the submission does sits *inside* the idempotency run,
    // nullifier reservation included: a resubmission under a known key must
    // replay the first answer, and reserving here would refuse it as a
    // double-spend before it got that far.
    let idempotency = st.idempotency.clone();
    let tx_hash = idempotency
        .run(
            chain_id,
            idempotency_key(&headers),
            submit_once(st, pipeline, payload),
        )
        .await?;

    Ok(Json(RelayerSubmitResponse { tx_hash }))
}

/// One spend, from admission control to a transaction hash.
async fn submit_once(
    st: AppState,
    pipeline: Arc<SpendPipeline>,
    payload: SubmitSpendPayload,
) -> AppResult<String> {
    let chain_id = payload.chain_id;
    let nfs = [
        parse_b32(&payload.pub_inputs.nullifier[0])?.0,
        parse_b32(&payload.pub_inputs.nullifier[1])?.0,
    ];
    let nf_guard = st.nullifiers.reserve(&st.pool, chain_id, nfs).await?;

    let receipt = pipeline.process(payload).await?;

    // Landed. Hold these nullifiers against a resubmit until the indexer has
    // written them to `spent_nullifiers`.
    nf_guard.spent().await;
    Ok(format!("0x{}", hex::encode(receipt.tx_hash)))
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
