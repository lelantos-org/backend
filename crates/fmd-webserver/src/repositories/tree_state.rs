use crate::domain::error::AppResult;
use database::DbPool;
use database::models::TreeStateRow;
use database::schema::tree_state;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// The chain's stored tree state, or `None` before fmd-indexer has written one.
pub async fn load(pool: &DbPool, chain_id: i64) -> AppResult<Option<TreeStateRow>> {
    let mut conn = super::conn(pool).await?;
    tree_state::table
        .find(chain_id)
        .select(TreeStateRow::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(super::db_err)
}
