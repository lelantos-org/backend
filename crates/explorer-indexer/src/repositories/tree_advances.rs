use crate::error::ExplorerIndexerError;
use database::DbPool;
pub use database::models::TreeAdvanceRow;
use database::schema::tree_advances;
use diesel::prelude::*;
use diesel::sql_query;
use diesel_async::RunQueryDsl;

pub async fn insert(pool: &DbPool, row: TreeAdvanceRow) -> Result<usize, ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    Ok(diesel::insert_into(tree_advances::table)
        .values(&row)
        .on_conflict((
            tree_advances::chain_id,
            tree_advances::block_number,
            tree_advances::log_index,
        ))
        .do_nothing()
        .execute(&mut conn)
        .await?)
}

pub async fn refresh_hourly_mv(pool: &DbPool) -> Result<(), ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    sql_query("REFRESH MATERIALIZED VIEW CONCURRENTLY tree_advances_hourly")
        .execute(&mut conn)
        .await?;
    Ok(())
}

pub async fn delete_from_block(
    pool: &DbPool,
    chain_id: i64,
    from_block: i64,
) -> Result<usize, ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    Ok(diesel::delete(
        tree_advances::table
            .filter(tree_advances::chain_id.eq(chain_id))
            .filter(tree_advances::block_number.ge(from_block)),
    )
    .execute(&mut conn)
    .await?)
}
