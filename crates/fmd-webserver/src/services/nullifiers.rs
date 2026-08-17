use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::repositories::nullifiers;
use std::sync::Arc;

pub const CHUNK_SIZE: u64 = 1024;

/// Width of a stored nullifier. A row of any other width is a bug upstream,
/// not input to be handled.
const NF_BYTES: usize = 32;

/// Bytes of each nullifier put on the wire, taken from the low end.
///
/// The client only tests set membership, so the rest is dead weight on a feed
/// every wallet downloads in full — dropping it takes a chunk from ~70 KB to
/// ~26 KB. The spent set is bounded by the tree's `4^10` leaves, so the chance
/// a live note collides with it is `2^20 / 2^80 = 2^-60`; the cost if one ever
/// did is a wallet declining to spend that note, not a lost note.
///
/// Low bytes, not high: the top byte of a bn254 field element is biased by the
/// modulus, the low ones are uniform.
const WIRE_BYTES: usize = 10;

pub struct ChunkResponse {
    pub chunk_id: u64,
    /// 0x-prefixed hex, truncated per `WIRE_BYTES`, ascending by `seq`. The
    /// client holds the whole set and filters its own notes locally — the
    /// server never learns which nullifiers a wallet cares about.
    pub nullifiers: Vec<String>,
    pub is_complete: bool,
}

/// Truncating hex encoder for one stored nullifier. Rejects any other width:
/// slicing the tail off a short row would silently emit a value no client
/// could match.
fn nf_to_hex(nf: &[u8]) -> AppResult<String> {
    let nf: &[u8; NF_BYTES] = nf.try_into().map_err(|_| {
        AppError::Internal(format!(
            "spent nullifier is {} bytes, expected {NF_BYTES}",
            nf.len()
        ))
    })?;
    Ok(format!("0x{}", hex::encode(&nf[NF_BYTES - WIRE_BYTES..])))
}

pub async fn get_chunk(
    st: &AppState,
    chain_id: i64,
    chunk_id: u64,
) -> AppResult<Arc<ChunkResponse>> {
    // Only complete (immutable) chunks are cached; see `AppCache::nullifier_chunks`.
    if let Some(cached) = st.cache.nullifier_chunks.get(&(chain_id, chunk_id)).await {
        return Ok(cached);
    }

    let from = (chunk_id * CHUNK_SIZE) as i64;
    let to = from + CHUNK_SIZE as i64;
    let rows = nullifiers::list_chunk(&st.pool, chain_id, from, to).await?;
    let is_complete = rows.len() as u64 == CHUNK_SIZE;
    let nullifiers = rows
        .iter()
        .map(Vec::as_slice)
        .map(nf_to_hex)
        .collect::<AppResult<Vec<_>>>()?;
    let response = Arc::new(ChunkResponse {
        chunk_id,
        nullifiers,
        is_complete,
    });

    if is_complete {
        st.cache
            .nullifier_chunks
            .insert((chain_id, chunk_id), Arc::clone(&response))
            .await;
    }

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_low_bytes() {
        let nf: [u8; NF_BYTES] = std::array::from_fn(|i| i as u8);
        // Bytes 22..32 of the big-endian encoding, i.e. the low 80 bits.
        assert_eq!(nf_to_hex(&nf).unwrap(), "0x161718191a1b1c1d1e1f");
    }

    #[test]
    fn pads_small_values_to_full_width() {
        // A short encoding would change the byte length the client reads back.
        let mut nf = [0u8; NF_BYTES];
        nf[NF_BYTES - 1] = 1;
        assert_eq!(nf_to_hex(&nf).unwrap(), "0x00000000000000000001");
    }

    #[test]
    fn rejects_rows_of_any_other_width() {
        // Slicing the tail off a short row yields a differently-aligned value
        // that no client could ever match, so it must fail loudly.
        assert!(nf_to_hex(&[0u8; NF_BYTES - 1]).is_err());
        assert!(nf_to_hex(&[0u8; NF_BYTES + 1]).is_err());
        assert!(nf_to_hex(&[]).is_err());
    }
}
