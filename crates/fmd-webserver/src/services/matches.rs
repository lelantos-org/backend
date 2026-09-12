//! The token-scoped match feed behind `/v1/matches`.

use crate::app::AppState;
use crate::app::cache::MatchesPageKey;
use crate::domain::error::AppResult;
use crate::domain::field::pack_point_hex;
use crate::domain::responses::{MatchOut, MatchesPage};
use crate::repositories::matches;
use crate::services::cached;
use std::sync::Arc;

/// One request for a page of matches.
///
/// Grouped rather than passed as five positional `i64`s, which the compiler
/// cannot distinguish: swapping `after` with `limit`, or `chain_id` with
/// `backfilled_through`, would type-check and serve wrong data.
#[derive(Debug, Clone, Copy)]
pub struct ListRequest {
    pub subscription_id: i64,
    /// Scopes the feed. A subscription is not chain-scoped, so without this the
    /// caller receives notes from every chain it matched on.
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

/// `backfilled_through` rides in the cached value, so it can trail the row by up
/// to the cache TTL. Clients clamp their cursor to it, and a value that is too
/// low re-delivers rows rather than skipping them.
#[tracing::instrument(skip(st))]
pub async fn list(st: &AppState, req: ListRequest) -> AppResult<Arc<MatchesPage>> {
    let pool = st.pool.clone();
    cached(
        &st.cache.matches_pages,
        "matches_pages",
        req.cache_key(),
        async move {
            let rows = matches::list_for_subscription(
                &pool,
                req.subscription_id,
                req.chain_id,
                req.after,
                req.limit,
            )
            .await?;
            let out = rows
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
            Ok(Arc::new(MatchesPage {
                backfilled_through_note_id: req.backfilled_through,
                matches: out,
            }))
        },
    )
    .await
}
