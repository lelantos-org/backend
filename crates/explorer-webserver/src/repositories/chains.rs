use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use database::schema::chain_state;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Every chain the ingester scans, ascending.
///
/// `chain_state` rather than `assets`: a chain is indexed from the moment it is
/// scanned, whether or not an asset has registered on it yet, and this list is
/// what separates "indexed and quiet" from "not indexed at all" for a caller
/// that would otherwise only see the chains that happen to have activity.
pub async fn indexed(pool: &DbPool) -> AppResult<Vec<i64>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    chain_state::table
        .select(chain_state::chain_id)
        .order(chain_state::chain_id.asc())
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
