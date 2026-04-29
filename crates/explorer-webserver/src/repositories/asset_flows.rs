use crate::domain::error::{AppError, AppResult};
use bigdecimal::BigDecimal;
use database::DbPool;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Nullable, Numeric};
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, QueryableByName)]
pub struct FlowBucketRow {
    #[diesel(sql_type = BigInt)]
    pub ts: i64,
    #[diesel(sql_type = Numeric)]
    pub in_amount: BigDecimal,
    #[diesel(sql_type = Numeric)]
    pub out_amount: BigDecimal,
}

pub async fn flow_buckets(
    pool: &DbPool,
    chain_id: Option<i64>,
    asset_id_u64: Option<i64>,
    bucket_sec: i64,
    since_ts: Option<i64>,
) -> AppResult<Vec<FlowBucketRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    sql_query(
        "SELECT (ts_hour / $1) * $1 AS ts, \
                SUM(in_amount)::NUMERIC(78, 0)  AS in_amount, \
                SUM(out_amount)::NUMERIC(78, 0) AS out_amount \
         FROM asset_flows_hourly \
         WHERE ($2::BIGINT IS NULL OR chain_id = $2) \
           AND ($3::BIGINT IS NULL OR asset_id_u64 = $3) \
           AND ($4::BIGINT IS NULL OR ts_hour >= $4) \
         GROUP BY 1 \
         ORDER BY 1",
    )
    .bind::<BigInt, _>(bucket_sec)
    .bind::<Nullable<BigInt>, _>(chain_id)
    .bind::<Nullable<BigInt>, _>(asset_id_u64)
    .bind::<Nullable<BigInt>, _>(since_ts)
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}
