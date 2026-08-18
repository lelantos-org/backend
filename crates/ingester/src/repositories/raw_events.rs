use crate::domain::error::IngesterError;
use crate::repositories::checkout;
use async_trait::async_trait;
use database::DbPool;
use database::schema::raw_events;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::Text;
use diesel_async::RunQueryDsl;

/// Channel announcing newly appended rows.
pub const CHANNEL_APPENDED: &str = "raw_events_appended";
/// Channel announcing that rows at or above a height were withdrawn.
///
/// Consumers stream `raw_events` by ascending `id`, so re-inserted canonical
/// rows are picked up naturally — but state they already derived from the
/// orphaned rows is invisible to that cursor and has to be retracted
/// explicitly. Payload is `<chain_id>:<rewind_to>`.
///
/// This is a latency optimisation, not the mechanism: `chain_reorgs` is the
/// durable record, because a NOTIFY sent while a consumer is down is simply
/// lost.
pub const CHANNEL_REORG: &str = "raw_events_reorg";

#[async_trait]
pub trait RawEventRepo: Send + Sync {
    /// Distinct `(block_number, block_hash)` in `[from_block, to_block]`,
    /// highest block first. Drives the reorg anchor walk.
    async fn block_hashes_desc(
        &self,
        chain_id: i64,
        from_block: i64,
        to_block: i64,
    ) -> Result<Vec<(i64, Vec<u8>)>, IngesterError>;
    async fn notify_appended(&self, chain_id: i64) -> Result<(), IngesterError>;
    async fn notify_reorg(&self, chain_id: i64, rewind_to: i64) -> Result<(), IngesterError>;
}

pub struct PostgresRawEventRepo {
    pool: DbPool,
}

impl PostgresRawEventRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    async fn notify(&self, channel: &str, payload: String) -> Result<(), IngesterError> {
        let mut conn = checkout(&self.pool).await?;
        sql_query("SELECT pg_notify($1, $2)")
            .bind::<Text, _>(channel.to_string())
            .bind::<Text, _>(payload)
            .execute(&mut conn)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl RawEventRepo for PostgresRawEventRepo {
    async fn block_hashes_desc(
        &self,
        chain_id: i64,
        from_block: i64,
        to_block: i64,
    ) -> Result<Vec<(i64, Vec<u8>)>, IngesterError> {
        let mut conn = checkout(&self.pool).await?;
        Ok(raw_events::table
            .filter(raw_events::chain_id.eq(chain_id))
            .filter(raw_events::block_number.between(from_block, to_block))
            .distinct_on(raw_events::block_number)
            .order(raw_events::block_number.desc())
            .select((raw_events::block_number, raw_events::block_hash))
            .load(&mut conn)
            .await?)
    }

    async fn notify_appended(&self, chain_id: i64) -> Result<(), IngesterError> {
        self.notify(CHANNEL_APPENDED, chain_id.to_string()).await
    }

    async fn notify_reorg(&self, chain_id: i64, rewind_to: i64) -> Result<(), IngesterError> {
        self.notify(CHANNEL_REORG, format!("{}:{}", chain_id, rewind_to))
            .await
    }
}
