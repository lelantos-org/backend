use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use database::schema::chain_state;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Every chain the ingester scans, ascending.
///
/// Reads `chain_state` rather than `assets`: a chain is indexed from the moment
/// it is scanned, whether or not an asset has registered on it. This list
/// separates an indexed but quiet chain from an unindexed one for callers that
/// would otherwise see only chains with activity.
pub async fn indexed(pool: &DbPool) -> AppResult<Vec<i64>> {
    let mut conn = super::conn(pool).await?;
    chain_state::table
        .select(chain_state::chain_id)
        .order(chain_state::chain_id.asc())
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
