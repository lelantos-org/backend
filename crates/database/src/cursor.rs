//! Shared cursor repository.
//!
//! All indexers (and webserver tools that read cursors) consume the same
//! `consumer_cursors` table. Trait + Postgres impl live here so each crate
//! depends on a single source of truth.

use crate::DbPool;
use crate::schema::{chain_state, consumer_cursors};
use async_trait::async_trait;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CursorError {
    #[error("pool: {0}")]
    Pool(String),
    #[error("query: {0}")]
    Query(#[from] diesel::result::Error),
}

pub type CursorResult<T> = Result<T, CursorError>;

#[derive(Debug, Clone, Insertable, AsChangeset)]
#[diesel(table_name = consumer_cursors)]
pub struct UpsertCursor {
    pub name: String,
    pub chain_id: i64,
    pub last_event_id: i64,
    pub last_block_number: i64,
}

#[async_trait]
pub trait CursorRepo: Send + Sync {
    async fn fetch(&self, name: &str, chain_id: i64) -> CursorResult<(i64, i64)>;
    async fn upsert(&self, row: UpsertCursor) -> CursorResult<()>;
    async fn list_chain_ids(&self) -> CursorResult<Vec<i64>>;
}

pub struct PostgresCursorRepo {
    pool: DbPool,
}

impl PostgresCursorRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CursorRepo for PostgresCursorRepo {
    async fn fetch(&self, name: &str, chain_id: i64) -> CursorResult<(i64, i64)> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| CursorError::Pool(e.to_string()))?;
        let row: Option<(i64, i64)> = consumer_cursors::table
            .filter(consumer_cursors::name.eq(name))
            .filter(consumer_cursors::chain_id.eq(chain_id))
            .select((
                consumer_cursors::last_event_id,
                consumer_cursors::last_block_number,
            ))
            .first(&mut conn)
            .await
            .optional()?;
        Ok(row.unwrap_or((0, 0)))
    }

    async fn upsert(&self, row: UpsertCursor) -> CursorResult<()> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| CursorError::Pool(e.to_string()))?;
        diesel::insert_into(consumer_cursors::table)
            .values(&row)
            .on_conflict((consumer_cursors::name, consumer_cursors::chain_id))
            .do_update()
            .set(&row)
            .execute(&mut conn)
            .await?;
        Ok(())
    }

    async fn list_chain_ids(&self) -> CursorResult<Vec<i64>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| CursorError::Pool(e.to_string()))?;
        Ok(chain_state::table
            .select(chain_state::chain_id)
            .load(&mut conn)
            .await?)
    }
}
