use crate::app::AppState;
use crate::app::cache::MatchesPageKey;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{MatchOut, MatchesPage};
use crate::repositories::matches;
use crate::services::field::pack_point_hex;
use std::sync::Arc;

/// One request for a page of matches.
///
/// Grouped rather than passed as five positional `i64`s, which no compiler
/// check distinguishes: swapping `after` with `limit`, or `chain_id` with
/// `backfilled_through`, would type-check and quietly serve wrong data.
#[derive(Debug, Clone, Copy)]
pub struct ListRequest {
    pub subscription_id: i64,
    /// Scopes the feed. A subscription is not chain-scoped, so without this
    /// the caller receives notes from every chain it matched on.
    pub chain_id: i64,
    /// Highest note id known to be backfilled for this subscription.
    pub backfilled_through: i64,
    pub after: i64,
    pub limit: i64,
}

impl ListRequest {
    fn cache_key(&self) -> MatchesPageKey {
        MatchesPageKey {
            subscription_id: self.subscription_id,
            chain_id: self.chain_id,
            after: self.after,
            limit: self.limit,
        }
    }
}

/// `backfilled_through` rides in the cached value, so it can be up to the
/// cache TTL behind the row. That staleness is safe in the only direction
/// that matters: clients clamp their cursor to it, and a value that is too
/// low re-delivers rows rather than skipping them.
#[tracing::instrument(skip(st))]
pub async fn list(st: &AppState, req: ListRequest) -> AppResult<Arc<MatchesPage>> {
    let key = req.cache_key();
    let pool = st.pool.clone();
    let probe = shared::metrics::CacheProbe::new("matches_pages");
    let miss = probe.marker();
    let out = st
        .cache
        .matches_pages
        .try_get_with(key, async move {
            miss.mark();
            let rows = matches::list_for_subscription(
                &pool,
                req.subscription_id,
                req.chain_id,
                req.after,
                req.limit,
            )
            .await?;
            let out: Vec<MatchOut> = rows
                .into_iter()
                .map(|m| {
                    Ok(MatchOut {
                        note_id: m.note_id,
                        chain_id: m.chain_id,
                        block_number: m.block_number,
                        leaf_index: m.leaf_index,
                        commitment_hex: hex::encode(&m.cm),
                        ciphertext_hex: hex::encode(&m.ciphertext),
                        eph_pub_packed_hex: pack_point_hex(&m.eph_pub_x, &m.eph_pub_y)?,
                    })
                })
                .collect::<AppResult<Vec<_>>>()?;
            Ok::<_, AppError>(Arc::new(MatchesPage {
                backfilled_through_note_id: req.backfilled_through,
                matches: out,
            }))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()));
    // After the await, and on the error path too: a load that failed still
    // missed, and counting it as a hit would flatter the cache.
    probe.record();
    out
}
