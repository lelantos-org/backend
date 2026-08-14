use crate::domain::error::{FmdIndexerError, Result};
use async_trait::async_trait;
use database::DbPool;
use database::schema::raw_events;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Queryable, Selectable, QueryableByName)]
#[diesel(table_name = raw_events)]
pub struct RawEventRow {
    pub id: i64,
    pub chain_id: i64,
    pub block_number: i64,
    pub block_hash: Vec<u8>,
    pub block_ts: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub event_kind: i16,
    pub topics: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}

#[async_trait]
pub trait RawEventsRepo: Send + Sync {
    async fn batch_after(
        &self,
        chain_id: i64,
        after_id: i64,
        kinds: &[i16],
        limit: i64,
    ) -> Result<Vec<RawEventRow>>;
    async fn max_id(&self, chain_id: i64) -> Result<i64>;
    /// Look up `DepositEscrowed` events by their `deposit_id` (encoded as
    /// the second topic of the log). Used by the consume pipeline to
    /// resolve cm + aux when processing `DepositFlushed` events.
    async fn fetch_escrowed_by_ids(
        &self,
        chain_id: i64,
        deposit_ids: &[Vec<u8>],
    ) -> Result<Vec<RawEventRow>>;
}

pub struct PostgresRawEventsRepo {
    pool: DbPool,
}

impl PostgresRawEventsRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RawEventsRepo for PostgresRawEventsRepo {
    async fn batch_after(
        &self,
        chain_id: i64,
        after_id: i64,
        kinds: &[i16],
        limit: i64,
    ) -> Result<Vec<RawEventRow>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        let rows = raw_events::table
            .filter(raw_events::chain_id.eq(chain_id))
            .filter(raw_events::id.gt(after_id))
            .filter(raw_events::event_kind.eq_any(kinds))
            .order(raw_events::id.asc())
            .limit(limit)
            .select(RawEventRow::as_select())
            .load(&mut conn)
            .await?;
        Ok(rows)
    }

    async fn max_id(&self, chain_id: i64) -> Result<i64> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        let v: Option<i64> = raw_events::table
            .filter(raw_events::chain_id.eq(chain_id))
            .select(diesel::dsl::max(raw_events::id))
            .first(&mut conn)
            .await?;
        Ok(v.unwrap_or(0))
    }

    async fn fetch_escrowed_by_ids(
        &self,
        chain_id: i64,
        deposit_ids: &[Vec<u8>],
    ) -> Result<Vec<RawEventRow>> {
        if deposit_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        // Postgres arrays are 1-based: topics[2] is the second topic (indexed deposit id, 32B big-endian).
        let kind = shared::entities::EventKind::DepositEscrowed.as_i16();
        let q = diesel::sql_query(
            "SELECT id, chain_id, block_number, block_hash, block_ts, tx_hash, log_index, event_kind, topics, data \
             FROM raw_events \
             WHERE chain_id = $1 AND event_kind = $2 AND topics[2] = ANY($3)",
        )
        .bind::<diesel::sql_types::BigInt, _>(chain_id)
        .bind::<diesel::sql_types::SmallInt, _>(kind)
        .bind::<diesel::sql_types::Array<diesel::sql_types::Bytea>, _>(deposit_ids.to_vec());
        let rows: Vec<RawEventRow> = q.load(&mut conn).await?;
        Ok(rows)
    }
}
