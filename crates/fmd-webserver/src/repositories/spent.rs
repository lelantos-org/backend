use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use database::schema::spent_nullifiers;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Return the subset of `nfs` that is recorded in `spent_nullifiers` for
/// the given chain. Order is not preserved; caller deduplicates.
pub async fn subset(pool: &DbPool, chain_id: i64, nfs: Vec<Vec<u8>>) -> AppResult<Vec<Vec<u8>>> {
    if nfs.is_empty() {
        return Ok(vec![]);
    }
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    spent_nullifiers::table
        .filter(spent_nullifiers::chain_id.eq(chain_id))
        .filter(spent_nullifiers::nf.eq_any(nfs))
        .select(spent_nullifiers::nf)
        .load::<Vec<u8>>(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
