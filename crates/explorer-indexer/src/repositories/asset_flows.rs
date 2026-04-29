use crate::error::ExplorerIndexerError;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::asset_flows;
use diesel::prelude::*;
use diesel::sql_query;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = asset_flows)]
pub struct NewAssetFlow {
    pub chain_id: i64,
    pub block_number: i64,
    pub log_index: i32,
    pub asset_id_u64: i64,
    pub token: Vec<u8>,
    pub in_amount: BigDecimal,
    pub out_amount: BigDecimal,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}

pub async fn refresh_hourly_mv(pool: &DbPool) -> Result<(), ExplorerIndexerError> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))?;
    sql_query("REFRESH MATERIALIZED VIEW CONCURRENTLY asset_flows_hourly")
        .execute(&mut conn)
        .await?;
    Ok(())
}

pub async fn insert(pool: &DbPool, row: NewAssetFlow) -> Result<usize, ExplorerIndexerError> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))?;
    Ok(diesel::insert_into(asset_flows::table)
        .values(&row)
        .on_conflict((
            asset_flows::chain_id,
            asset_flows::block_number,
            asset_flows::log_index,
        ))
        .do_nothing()
        .execute(&mut conn)
        .await?)
}
