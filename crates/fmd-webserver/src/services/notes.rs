//! The public note feed behind `/v1/notes`.

use crate::app::AppState;
use crate::app::cache::NotesPageKey;
use crate::domain::error::AppResult;
use crate::domain::point::pack_point_hex;
use crate::domain::responses::NoteOut;
use crate::repositories::notes;
use crate::services::cached;
use std::sync::Arc;

#[tracing::instrument(skip(st))]
pub async fn list(
    st: &AppState,
    chain_id: Option<i64>,
    after: i64,
    limit: i64,
) -> AppResult<Arc<Vec<NoteOut>>> {
    let key = NotesPageKey {
        chain_id,
        after,
        limit,
    };
    let pool = st.pool.clone();
    cached(&st.cache.notes_pages, "notes_pages", key, async move {
        let rows = notes::list(&pool, chain_id, after, limit).await?;
        let out = rows
            .into_iter()
            .map(|n| {
                Ok(NoteOut {
                    id: n.id,
                    chain_id: n.chain_id,
                    block_number: n.block_number,
                    leaf_index: n.leaf_index,
                    commitment_hex: hex::encode(&n.cm),
                    ciphertext_hex: hex::encode(&n.ciphertext),
                    eph_pub_packed_hex: pack_point_hex(&n.eph_pub_x, &n.eph_pub_y)?,
                })
            })
            .collect::<AppResult<Vec<_>>>()?;
        Ok(Arc::new(out))
    })
    .await
}
