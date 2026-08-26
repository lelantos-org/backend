use crate::domain::error::Result;
use async_trait::async_trait;
use database::DbPool;
pub use database::models::RawEventRow;
use database::schema::raw_events;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

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
    /// Look up `DepositEscrowed` events by `deposit_id`, encoded as the second
    /// topic of the log. The consume pipeline uses this to resolve cm and aux
    /// when processing `DepositFlushed` events.
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
        let mut conn = super::conn(&self.pool).await?;
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
        let mut conn = super::conn(&self.pool).await?;
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
        let mut conn = super::conn(&self.pool).await?;
        // Postgres arrays are 1-based, so topics[2] is the second topic: the
        // indexed deposit id, 32 bytes big-endian. Ordered by id so a re-used
        // deposit id resolves to the same escrow on every replica; served by
        // `raw_events_escrowed_id_idx`.
        let kind = shared::entities::EventKind::DepositEscrowed.as_i16();
        let q = diesel::sql_query(
            "SELECT id, chain_id, block_number, block_hash, block_ts, tx_hash, log_index, event_kind, topics, data \
             FROM raw_events \
             WHERE chain_id = $1 AND event_kind = $2 AND topics[2] = ANY($3) \
             ORDER BY id ASC",
        )
        .bind::<diesel::sql_types::BigInt, _>(chain_id)
        .bind::<diesel::sql_types::SmallInt, _>(kind)
        .bind::<diesel::sql_types::Array<diesel::sql_types::Bytea>, _>(deposit_ids.to_vec());
        let rows: Vec<RawEventRow> = q.load(&mut conn).await?;
        Ok(rows)
    }
}
