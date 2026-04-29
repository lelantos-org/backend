use crate::domain::error::IngesterError;
use crate::domain::models::BlockCursor;
use async_trait::async_trait;
use database::DbPool;
use database::schema::chain_state;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[async_trait]
pub trait ChainStateRepo: Send + Sync {
    async fn fetch(&self, chain_id: i64) -> Result<Option<BlockCursor>, IngesterError>;
    async fn upsert(&self, cursor: BlockCursor) -> Result<(), IngesterError>;
    async fn advance_scanned(&self, chain_id: i64, scanned: i64) -> Result<(), IngesterError>;
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = chain_state)]
struct ChainStateRow {
    chain_id: i64,
    last_block: i64,
    last_block_hash: Vec<u8>,
    last_scanned_block: i64,
}

impl From<ChainStateRow> for BlockCursor {
    fn from(r: ChainStateRow) -> Self {
        BlockCursor {
            chain_id: r.chain_id,
            last_block: r.last_block,
            last_block_hash: r.last_block_hash,
            last_scanned_block: r.last_scanned_block,
        }
    }
}

#[derive(Insertable, AsChangeset)]
#[diesel(table_name = chain_state)]
struct ChainStateUpsertRow {
    chain_id: i64,
    last_block: i64,
    last_block_hash: Vec<u8>,
    last_scanned_block: i64,
}

impl From<BlockCursor> for ChainStateUpsertRow {
    fn from(c: BlockCursor) -> Self {
        Self {
            chain_id: c.chain_id,
            last_block: c.last_block,
            last_block_hash: c.last_block_hash,
            last_scanned_block: c.last_scanned_block,
        }
    }
}

pub struct PostgresChainStateRepo {
    pool: DbPool,
}

impl PostgresChainStateRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ChainStateRepo for PostgresChainStateRepo {
    async fn fetch(&self, chain_id: i64) -> Result<Option<BlockCursor>, IngesterError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        let row: Option<ChainStateRow> = chain_state::table
            .filter(chain_state::chain_id.eq(chain_id))
            .select(ChainStateRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        Ok(row.map(BlockCursor::from))
    }

    async fn upsert(&self, cursor: BlockCursor) -> Result<(), IngesterError> {
        let row: ChainStateUpsertRow = cursor.into();
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        diesel::insert_into(chain_state::table)
            .values(&row)
            .on_conflict(chain_state::chain_id)
            .do_update()
            .set(&row)
            .execute(&mut conn)
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        Ok(())
    }

    async fn advance_scanned(&self, chain_id: i64, scanned: i64) -> Result<(), IngesterError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        diesel::update(chain_state::table.filter(chain_state::chain_id.eq(chain_id)))
            .set(chain_state::last_scanned_block.eq(scanned))
            .execute(&mut conn)
            .await
            .map_err(|e| IngesterError::Db(e.to_string()))?;
        Ok(())
    }
}
