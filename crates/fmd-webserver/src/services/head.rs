use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::HeadOut;
use crate::repositories::{notes, nullifiers};

/// Read both watermarks for `chain_id`.
///
/// Uncached on purpose. A cache here would reintroduce exactly the staleness
/// this endpoint exists to remove, and both reads are indexed `MAX()`s — the
/// cache lookup would not be meaningfully cheaper than the query.
///
/// Concurrent rather than sequential: they touch different tables and a poll
/// this frequent should cost one round trip, not two.
pub async fn get(st: &AppState, chain_id: i64) -> AppResult<HeadOut> {
    let (max_note_id, max_nullifier_seq) = tokio::try_join!(
        notes::max_id(&st.pool, chain_id),
        nullifiers::max_seq(&st.pool, chain_id),
    )?;
    Ok(HeadOut {
        chain_id,
        max_note_id,
        max_nullifier_seq,
    })
}
