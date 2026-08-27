use crate::domain::error::IngesterError;
use crate::repositories::checkout;
use async_trait::async_trait;
use database::DbPool;
use database::schema::raw_events;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Reads `raw_events` back as block hashes, for the reorg anchor walk.
///
/// Named for what it serves rather than for the table: it is the seam the reorg
/// tests stand in for, and the walk is the only thing that reads this shape.
/// Writes go through [`crate::repositories::atomic`], which also emits the
/// wake-up NOTIFYs inside the transactions they announce.
#[async_trait]
pub trait BlockHashRepo: Send + Sync {
    /// Distinct `(block_number, block_hash)` in `[from_block, to_block]`, highest
    /// block first.
    async fn block_hashes_desc(
        &self,
        chain_id: i64,
        from_block: i64,
        to_block: i64,
    ) -> Result<Vec<(i64, Vec<u8>)>, IngesterError>;
}

pub struct PostgresBlockHashRepo {
    pool: DbPool,
}

impl PostgresBlockHashRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl BlockHashRepo for PostgresBlockHashRepo {
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
}
