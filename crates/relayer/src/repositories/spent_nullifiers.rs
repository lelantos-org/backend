use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use database::schema::spent_nullifiers;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Return the subset of `nfs` already recorded as spent for `chain_id`.
/// Mirrors `fmd-webserver::repositories::spent::subset` (kept local so the
/// relayer does not depend on the webserver crate).
pub async fn any_spent(pool: &DbPool, chain_id: i64, nfs: &[[u8; 32]]) -> AppResult<Vec<Vec<u8>>> {
    if nfs.is_empty() {
        return Ok(vec![]);
    }
    let bufs: Vec<Vec<u8>> = nfs.iter().map(|n| n.to_vec()).collect();
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    spent_nullifiers::table
        .filter(spent_nullifiers::chain_id.eq(chain_id))
        .filter(spent_nullifiers::nf.eq_any(bufs))
        .select(spent_nullifiers::nf)
        .load::<Vec<u8>>(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
