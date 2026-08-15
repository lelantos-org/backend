use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::MatchOut;
use crate::repositories::matches;
use crate::services::field::bigdec_to_hex;
use std::sync::Arc;

fn clue_bits_hex(ciphertext: &[u8]) -> String {
    if ciphertext.len() < 2 {
        return "0x0000".to_string();
    }
    format!(
        "0x{:04x}",
        u16::from_be_bytes([ciphertext[0], ciphertext[1]])
    )
}

#[tracing::instrument(skip(st))]
pub async fn list(
    st: &AppState,
    subscription_id: i64,
    after: i64,
    limit: i64,
) -> AppResult<Arc<Vec<MatchOut>>> {
    let key = (subscription_id, after, limit);
    let pool = st.pool.clone();
    st.cache
        .matches_pages
        .try_get_with(key, async move {
            let rows = matches::list_for_subscription(&pool, subscription_id, after, limit).await?;
            let out: Vec<MatchOut> = rows
                .into_iter()
                .map(|m| {
                    Ok(MatchOut {
                        note_id: m.note_id,
                        chain_id: m.chain_id,
                        block_number: m.block_number,
                        leaf_index: m.leaf_index,
                        commitment_hex: hex::encode(&m.cm),
                        clue_bits_hex: clue_bits_hex(&m.ciphertext),
                        ciphertext_hex: hex::encode(&m.ciphertext),
                        eph_pub_x: bigdec_to_hex(&m.eph_pub_x)?,
                        eph_pub_y: bigdec_to_hex(&m.eph_pub_y)?,
                    })
                })
                .collect::<AppResult<Vec<_>>>()?;
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}
