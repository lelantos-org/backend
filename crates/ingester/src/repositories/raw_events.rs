use crate::domain::error::IngesterError;
use crate::domain::models::RawEvent;
use async_trait::async_trait;
use database::DbPool;
use database::schema::raw_events;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::Text;
use diesel_async::RunQueryDsl;

#[async_trait]
pub trait RawEventRepo: Send + Sync {
    async fn insert_batch(&self, rows: &[RawEvent]) -> Result<usize, IngesterError>;
    async fn delete_from_block(
        &self,
        chain_id: i64,
        from_block: i64,
    ) -> Result<usize, IngesterError>;
    async fn block_hash_at(
        &self,
        chain_id: i64,
        block_number: i64,
    ) -> Result<Option<Vec<u8>>, IngesterError>;
    async fn notify(&self, chain_id: i64) -> Result<(), IngesterError>;
}

#[derive(Insertable)]
#[diesel(table_name = raw_events)]
struct RawEventRow<'a> {
    chain_id: i64,
    block_number: i64,
    evm_block_number: Option<i64>,
    block_hash: &'a [u8],
    block_ts: i64,
    tx_hash: &'a [u8],
    log_index: i32,
    event_kind: i16,
    topics: &'a [Vec<u8>],
    data: &'a [u8],
}

pub struct PostgresRawEventRepo {
    pool: DbPool,
}

impl PostgresRawEventRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RawEventRepo for PostgresRawEventRepo {
    async fn insert_batch(&self, rows: &[RawEvent]) -> Result<usize, IngesterError> {
        if rows.is_empty() {
            return Ok(0);
        }
        let to_insert: Vec<RawEventRow> = rows
            .iter()
            .map(|r| RawEventRow {
                chain_id: r.chain_id,
                evm_block_number: Some(r.evm_block_number),
                block_number: r.block_number,
                block_hash: &r.block_hash,
                block_ts: r.block_ts,
                tx_hash: &r.tx_hash,
                log_index: r.log_index,
                event_kind: r.event_kind,
                topics: &r.topics,
                data: &r.data,
            })
            .collect();
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        let n = diesel::insert_into(raw_events::table)
            .values(&to_insert)
            .on_conflict((
                raw_events::chain_id,
                raw_events::block_number,
                raw_events::log_index,
            ))
            .do_nothing()
            .execute(&mut conn)
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        Ok(n)
    }

    async fn delete_from_block(
        &self,
        chain_id: i64,
        from_block: i64,
    ) -> Result<usize, IngesterError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        let n = diesel::delete(
            raw_events::table
                .filter(raw_events::chain_id.eq(chain_id))
                .filter(raw_events::block_number.ge(from_block)),
        )
        .execute(&mut conn)
        .await
        .map_err(|e| IngesterError::Db(e.to_string()))?;
        Ok(n)
    }

    async fn block_hash_at(
        &self,
        chain_id: i64,
        block_number: i64,
    ) -> Result<Option<Vec<u8>>, IngesterError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        let row: Option<Vec<u8>> = raw_events::table
            .filter(raw_events::chain_id.eq(chain_id))
            .filter(raw_events::block_number.eq(block_number))
            .select(raw_events::block_hash)
            .first(&mut conn)
            .await
            .optional()
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        Ok(row)
    }

    async fn notify(&self, chain_id: i64) -> Result<(), IngesterError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        sql_query("SELECT pg_notify($1, $2)")
            .bind::<Text, _>("raw_events_appended")
            .bind::<Text, _>(chain_id.to_string())
            .execute(&mut conn)
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        Ok(())
    }
}
