use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use database::schema::spent_nullifiers;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// One `seq`-ordered slice of the spent set. `seq` is the dense per-chain ordinal
/// fmd-indexer assigns at insert; see `notes::list_chunk` for the `leaf_index`
/// equivalent.
pub async fn list_chunk(
    pool: &DbPool,
    chain_id: i64,
    from_seq: i64,
    to_seq: i64,
) -> AppResult<Vec<Vec<u8>>> {
    let mut conn = super::conn(pool).await?;
    spent_nullifiers::table
        .filter(spent_nullifiers::chain_id.eq(chain_id))
        .filter(spent_nullifiers::seq.ge(from_seq))
        .filter(spent_nullifiers::seq.lt(to_seq))
        .order(spent_nullifiers::seq.asc())
        .select(spent_nullifiers::nf)
        .load::<Vec<u8>>(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}

/// Highest `spent_nullifiers.seq` for `chain_id`, or 0 when the chain has none.
///
/// `seq` is dense and grows only at the tail, so this doubles as the chunk feed's
/// high-water mark.
pub async fn max_seq(pool: &DbPool, chain_id: i64) -> AppResult<i64> {
    let mut conn = super::conn(pool).await?;
    let max: Option<i64> = spent_nullifiers::table
        .filter(spent_nullifiers::chain_id.eq(chain_id))
        .select(diesel::dsl::max(spent_nullifiers::seq))
        .first(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))?;
    Ok(max.unwrap_or(0))
}
