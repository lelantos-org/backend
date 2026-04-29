use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::repositories::spent;

const MAX_BATCH: usize = 1024;

fn parse_nf_hex(s: &str) -> AppResult<Vec<u8>> {
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(stripped).map_err(|e| AppError::BadRequest(format!("nf hex: {e}")))?;
    if bytes.len() != 32 {
        return Err(AppError::BadRequest(format!(
            "nf must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// Resolve which of the requested nullifiers are spent on chain.
///
/// Two-tier lookup:
/// 1. Probe positive cache (`AppCache.spent`) — spent bit is monotonic, so
///    a hit is authoritative and skips the DB.
/// 2. For misses, query `spent_nullifiers` once via `IN (…)`.
/// 3. Hits from the DB pass round are inserted into the cache for next time.
#[tracing::instrument(skip(st, nullifiers_hex), fields(chain_id, n = nullifiers_hex.len()))]
pub async fn resolve(
    st: &AppState,
    chain_id: i64,
    nullifiers_hex: Vec<String>,
) -> AppResult<Vec<String>> {
    if nullifiers_hex.len() > MAX_BATCH {
        return Err(AppError::BadRequest(format!(
            "too many nullifiers: {} > {}",
            nullifiers_hex.len(),
            MAX_BATCH
        )));
    }

    let mut parsed: Vec<Vec<u8>> = Vec::with_capacity(nullifiers_hex.len());
    for s in &nullifiers_hex {
        parsed.push(parse_nf_hex(s)?);
    }
    parsed.sort();
    parsed.dedup();

    let mut hits: Vec<Vec<u8>> = Vec::new();
    let mut to_query: Vec<Vec<u8>> = Vec::with_capacity(parsed.len());

    for nf in parsed {
        if st.cache.spent.contains_key(&(chain_id, nf.clone())) {
            hits.push(nf);
        } else {
            to_query.push(nf);
        }
    }

    if !to_query.is_empty() {
        let from_db = spent::subset(&st.pool, chain_id, to_query).await?;
        for nf in &from_db {
            st.cache.spent.insert((chain_id, nf.clone()), ()).await;
        }
        hits.extend(from_db);
    }

    Ok(hits
        .into_iter()
        .map(|nf| format!("0x{}", hex::encode(nf)))
        .collect())
}
