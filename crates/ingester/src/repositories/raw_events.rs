use crate::domain::error::IngesterError;
use crate::repositories::checkout;
use async_trait::async_trait;
use database::DbPool;
use database::schema::raw_events;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

// The channel names live in `database` so consumers can subscribe without
// depending on this crate; the ingester is the only publisher, but nothing
// downstream needs to link it.
use database::listen::{self, CHANNEL_RAW_EVENTS_APPENDED, CHANNEL_RAW_EVENTS_REORG};

#[async_trait]
pub trait RawEventRepo: Send + Sync {
    /// Distinct `(block_number, block_hash)` in `[from_block, to_block]`, highest
    /// block first. Drives the reorg anchor walk.
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
        listen::notify(&mut conn, channel, &payload).await?;
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
        self.notify(CHANNEL_RAW_EVENTS_APPENDED, chain_id.to_string())
            .await
    }

    async fn notify_reorg(&self, chain_id: i64, rewind_to: i64) -> Result<(), IngesterError> {
        self.notify(
            CHANNEL_RAW_EVENTS_REORG,
            format!("{}:{}", chain_id, rewind_to),
        )
        .await
    }
}
