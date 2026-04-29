use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use database::schema::tree_advances;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Integer, Nullable};
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = tree_advances)]
pub struct TreeAdvanceRow {
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

pub async fn list(
    pool: &DbPool,
    chain_id: Option<i64>,
    since_start_index: Option<i64>,
    limit: i64,
) -> AppResult<Vec<TreeAdvanceRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    let mut q = tree_advances::table.into_boxed();
    if let Some(c) = chain_id {
        q = q.filter(tree_advances::chain_id.eq(c));
    }
    if let Some(s) = since_start_index {
        q = q.filter(tree_advances::start_index.gt(s));
    }
    q.order((
        tree_advances::chain_id.asc(),
        tree_advances::start_index.asc(),
    ))
    .limit(limit)
    .select(TreeAdvanceRow::as_select())
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}

#[derive(Debug, Clone, QueryableByName)]
pub struct CountBucketRow {
    #[diesel(sql_type = BigInt)]
    pub ts: i64,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

pub async fn count_buckets(
    pool: &DbPool,
    chain_id: Option<i64>,
    bucket_sec: i64,
    since_ts: Option<i64>,
) -> AppResult<Vec<CountBucketRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    sql_query(
        "SELECT (ts_hour / $1) * $1 AS ts, \
                SUM(tx_count)::BIGINT AS count \
         FROM tree_advances_hourly \
         WHERE ($2::BIGINT IS NULL OR chain_id = $2) \
           AND ($3::BIGINT IS NULL OR ts_hour >= $3) \
         GROUP BY 1 \
         ORDER BY 1",
    )
    .bind::<BigInt, _>(bucket_sec)
    .bind::<Nullable<BigInt>, _>(chain_id)
    .bind::<Nullable<BigInt>, _>(since_ts)
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}

#[derive(Debug, Clone, QueryableByName)]
pub struct ChainFlow24hRow {
    #[diesel(sql_type = BigInt)]
    pub chain_id: i64,
    #[diesel(sql_type = Integer)]
    pub slot: i32,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

pub async fn chain_flows_24h(pool: &DbPool, hour_start: i64) -> AppResult<Vec<ChainFlow24hRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    sql_query(
        "SELECT chain_id, \
                ((ts_hour - $1) / 3600)::INT AS slot, \
                tx_count AS count \
         FROM tree_advances_hourly \
         WHERE ts_hour >= $1 \
         ORDER BY chain_id, slot",
    )
    .bind::<BigInt, _>(hour_start)
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}
