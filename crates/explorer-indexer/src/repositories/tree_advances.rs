use crate::error::ExplorerIndexerError;
use database::DbPool;
use database::schema::tree_advances;
use diesel::prelude::*;
use diesel::sql_query;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = tree_advances)]
pub struct NewTreeAdvance {
    pub chain_id: i64,
    pub block_number: i64,
    pub log_index: i32,
    pub start_index: i64,
    pub inserted: i32,
    pub old_root: Vec<u8>,
    pub new_root: Vec<u8>,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}

pub async fn insert(pool: &DbPool, row: NewTreeAdvance) -> Result<usize, ExplorerIndexerError> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))?;
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
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))?;
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
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))?;
    Ok(diesel::delete(
        tree_advances::table
            .filter(tree_advances::chain_id.eq(chain_id))
            .filter(tree_advances::block_number.ge(from_block)),
    )
    .execute(&mut conn)
    .await?)
}
