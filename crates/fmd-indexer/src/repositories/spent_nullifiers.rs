use crate::domain::error::{FmdIndexerError, Result};
use async_trait::async_trait;
use database::DbPool;
use database::schema::spent_nullifiers;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = spent_nullifiers)]
pub struct NewSpentNullifier {
    pub chain_id: i64,
    pub block_number: i64,
    pub log_index: i32,
    pub nf: Vec<u8>,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}

#[async_trait]
pub trait SpentNullifiersRepo: Send + Sync {
    async fn insert_batch(&self, rows: &[NewSpentNullifier]) -> Result<usize>;
    async fn delete_from_block(&self, chain_id: i64, from_block: i64) -> Result<usize>;
}

pub struct PostgresSpentNullifiersRepo {
    pool: DbPool,
}

impl PostgresSpentNullifiersRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SpentNullifiersRepo for PostgresSpentNullifiersRepo {
    async fn insert_batch(&self, rows: &[NewSpentNullifier]) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        let n = diesel::insert_into(spent_nullifiers::table)
            .values(rows)
            .on_conflict((
                spent_nullifiers::chain_id,
                spent_nullifiers::block_number,
                spent_nullifiers::log_index,
            ))
            .do_nothing()
            .execute(&mut conn)
            .await?;
        Ok(n)
    }

    async fn delete_from_block(&self, chain_id: i64, from_block: i64) -> Result<usize> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        let n = diesel::delete(
            spent_nullifiers::table
                .filter(spent_nullifiers::chain_id.eq(chain_id))
                .filter(spent_nullifiers::block_number.ge(from_block)),
        )
        .execute(&mut conn)
        .await?;
        Ok(n)
    }
}
