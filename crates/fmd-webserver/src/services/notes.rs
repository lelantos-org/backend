use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::NoteOut;
use crate::repositories::notes;
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
    chain_id: Option<i64>,
    after: i64,
    limit: i64,
) -> AppResult<Arc<Vec<NoteOut>>> {
    let key = (chain_id, after, limit);
    let pool = st.pool.clone();
    st.cache
        .notes_pages
        .try_get_with(key, async move {
            let rows = notes::list(&pool, chain_id, after, limit).await?;
            let out: Vec<NoteOut> = rows
                .into_iter()
                .map(|n| {
                    Ok(NoteOut {
                        id: n.id,
                        chain_id: n.chain_id,
                        block_number: n.block_number,
                        leaf_index: n.leaf_index,
                        commitment_hex: hex::encode(&n.cm),
                        clue_bits_hex: clue_bits_hex(&n.ciphertext),
                        ciphertext_hex: hex::encode(&n.ciphertext),
                        eph_pub_x: bigdec_to_hex(&n.eph_pub_x)?,
                        eph_pub_y: bigdec_to_hex(&n.eph_pub_y)?,
                    })
                })
                .collect::<AppResult<Vec<_>>>()?;
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}
