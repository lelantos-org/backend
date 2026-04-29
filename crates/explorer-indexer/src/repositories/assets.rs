use crate::error::ExplorerIndexerError;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::assets;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Insertable, AsChangeset)]
#[diesel(table_name = assets)]
pub struct UpsertAsset {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub token: Vec<u8>,
    pub scale: BigDecimal,
}

pub async fn upsert(pool: &DbPool, row: UpsertAsset) -> Result<(), ExplorerIndexerError> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))?;
    diesel::insert_into(assets::table)
        .values(&row)
        .on_conflict((assets::chain_id, assets::asset_id_u64))
        .do_update()
        .set(&row)
        .execute(&mut conn)
        .await?;
    Ok(())
}
