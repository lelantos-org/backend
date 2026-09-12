//! The spent-nullifier chunk feed.
//!
//! Entries are truncated to their low `WIRE_BYTES`: the client only tests set
//! membership, and every wallet downloads the feed whole.

use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{NullifierChunkOut, RenderedChunk};
use crate::repositories::nullifiers;
use crate::services::chunks;

pub use crate::services::chunks::CHUNK_SIZE;

/// Width of a stored nullifier. A row of any other width is an upstream bug
/// rather than input to be handled.
const NF_BYTES: usize = 32;

/// Bytes of each nullifier put on the wire, taken from the low end.
///
/// The client only tests set membership, so the remaining bytes are overhead on
/// a feed every wallet downloads in full; dropping them takes a chunk from ~70 KB
/// to ~26 KB. The spent set is bounded by the tree's `4^10` leaves, so a live
/// note collides with probability `2^20 / 2^80 = 2^-60`, and the consequence
/// would be a wallet declining to spend that note rather than losing it.
///
/// Low bytes rather than high: the top byte of a bn254 field element is biased by
/// the modulus while the low ones are uniform.
const WIRE_BYTES: usize = 10;

/// Truncating hex encoder for one stored nullifier. Rejects any other width,
/// since slicing the tail off a short row would emit a value no client could
/// match.
fn nf_to_hex(nf: &[u8]) -> AppResult<String> {
    let nf: &[u8; NF_BYTES] = nf.try_into().map_err(|_| {
        AppError::Internal(format!(
            "spent nullifier is {} bytes, expected {NF_BYTES}",
            nf.len()
        ))
    })?;
    Ok(format!("0x{}", hex::encode(&nf[NF_BYTES - WIRE_BYTES..])))
}

/// One chunk of the spent-nullifier feed, serialised and ready to write.
///
/// Rendered once for the same reason as the commitment feed: a complete chunk is
/// immutable, so its bytes are what the cache holds.
pub async fn get_chunk(st: &AppState, chain_id: i64, chunk_id: u64) -> AppResult<RenderedChunk> {
    chunks::serve(
        &st.cache.nullifier_chunks,
        "nullifier_chunks",
        (chain_id, chunk_id),
        render(st, chain_id, chunk_id),
    )
    .await
}

async fn render(st: &AppState, chain_id: i64, chunk_id: u64) -> AppResult<RenderedChunk> {
    let (from, to) = chunks::range(chunk_id);
    let rows = nullifiers::list_chunk(&st.pool, chain_id, from, to).await?;
    let is_complete = rows.len() as u64 == CHUNK_SIZE;
    let nullifiers = rows
        .iter()
        .map(Vec::as_slice)
        .map(nf_to_hex)
        .collect::<AppResult<Vec<_>>>()?;
    RenderedChunk::render(&NullifierChunkOut {
        chunk_id,
        nullifiers,
        is_complete,
    })
    .map_err(|e| AppError::Internal(format!("serialise nullifier chunk: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_low_bytes() {
        let nf: [u8; NF_BYTES] = std::array::from_fn(|i| i as u8);
        // Bytes 22..32 of the big-endian encoding, the low 80 bits.
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
        // Slicing the tail off a short row yields a differently aligned value no
        // client could match, so it must fail.
        assert!(nf_to_hex(&[0u8; NF_BYTES - 1]).is_err());
        assert!(nf_to_hex(&[0u8; NF_BYTES + 1]).is_err());
        assert!(nf_to_hex(&[]).is_err());
    }
}
