//! Multi-table writes that must land together.
//!
//! Rows and the cursor that describes them are two tables, so committing them
//! on two pooled connections leaves a window where one is written and the
//! other is not. Both composites here run inside a single transaction on a
//! single connection.

use crate::domain::error::IngesterError;
use crate::domain::models::{BlockCursor, RawEvent};
use crate::repositories::checkout;
use async_trait::async_trait;
use database::DbPool;
use database::schema::{chain_state, raw_events};
use diesel::prelude::*;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, RunQueryDsl};

/// Rows per `INSERT` statement.
///
/// Postgres caps a statement at 65535 bind parameters and each row binds 10,
/// so the hard ceiling is 6553 rows. A single backfill chunk of the default
/// 50k blocks can exceed that on a busy pool, which used to surface as an
/// opaque runtime error that killed the worker.
const INSERT_CHUNK_ROWS: usize = 1_000;

#[async_trait]
pub trait AtomicWriteRepo: Send + Sync {
    /// Insert `rows` and move the cursor to `cursor`, atomically.
    ///
    /// Returns the number of rows actually inserted; duplicates absorbed by
    /// the unique index do not count.
    async fn commit_batch(
        &self,
        rows: &[RawEvent],
        cursor: &BlockCursor,
    ) -> Result<usize, IngesterError>;

    /// Delete every row at or above `from_block`, reset the cursor, and
    /// record the rewind in the reorg log — all in one transaction.
    ///
    /// The log entry has to be atomic with the delete: consumers use it to
    /// retract state they derived from the rows being removed, and a marker
    /// that can go missing is worse than no marker at all.
    async fn rewind(
        &self,
        chain_id: i64,
        from_block: i64,
        cursor: &BlockCursor,
    ) -> Result<usize, IngesterError>;
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

impl<'a> From<&'a RawEvent> for RawEventRow<'a> {
    fn from(r: &'a RawEvent) -> Self {
        Self {
            chain_id: r.chain_id,
            block_number: r.block_number,
            evm_block_number: Some(r.evm_block_number),
            block_hash: &r.block_hash,
            block_ts: r.block_ts,
            tx_hash: &r.tx_hash,
            log_index: r.log_index,
            event_kind: r.event_kind,
            topics: &r.topics,
            data: &r.data,
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

impl From<&BlockCursor> for ChainStateUpsertRow {
    fn from(c: &BlockCursor) -> Self {
        Self {
            chain_id: c.chain_id,
            last_block: c.last_block,
            last_block_hash: c.last_block_hash.clone(),
            last_scanned_block: c.last_scanned_block,
        }
    }
}

pub struct PostgresAtomicWriteRepo {
    pool: DbPool,
}

impl PostgresAtomicWriteRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

async fn insert_rows(
    conn: &mut diesel_async::AsyncPgConnection,
    rows: &[RawEvent],
) -> Result<usize, diesel::result::Error> {
    let mut inserted = 0usize;
    for chunk in rows.chunks(INSERT_CHUNK_ROWS) {
        let values: Vec<RawEventRow<'_>> = chunk.iter().map(RawEventRow::from).collect();
        inserted += diesel::insert_into(raw_events::table)
            .values(&values)
            .on_conflict((
                raw_events::chain_id,
                raw_events::block_number,
                raw_events::log_index,
            ))
            .do_nothing()
            .execute(conn)
            .await?;
    }
    Ok(inserted)
}

async fn upsert_cursor(
    conn: &mut diesel_async::AsyncPgConnection,
    cursor: &BlockCursor,
) -> Result<(), diesel::result::Error> {
    let row = ChainStateUpsertRow::from(cursor);
    diesel::insert_into(chain_state::table)
        .values(&row)
        .on_conflict(chain_state::chain_id)
        .do_update()
        .set(&row)
        .execute(conn)
        .await?;
    Ok(())
}

#[async_trait]
impl AtomicWriteRepo for PostgresAtomicWriteRepo {
    async fn commit_batch(
        &self,
        rows: &[RawEvent],
        cursor: &BlockCursor,
    ) -> Result<usize, IngesterError> {
        let mut conn = checkout(&self.pool).await?;
        conn.transaction::<_, diesel::result::Error, _>(|conn| {
            async move {
                let n = insert_rows(conn, rows).await?;
                upsert_cursor(conn, cursor).await?;
                Ok(n)
            }
            .scope_boxed()
        })
        .await
        .map_err(IngesterError::from)
    }

    async fn rewind(
        &self,
        chain_id: i64,
        from_block: i64,
        cursor: &BlockCursor,
    ) -> Result<usize, IngesterError> {
        let mut conn = checkout(&self.pool).await?;
        conn.transaction::<_, diesel::result::Error, _>(|conn| {
            async move {
                let deleted = diesel::delete(
                    raw_events::table
                        .filter(raw_events::chain_id.eq(chain_id))
                        .filter(raw_events::block_number.ge(from_block)),
                )
                .execute(conn)
                .await?;
                upsert_cursor(conn, cursor).await?;
                database::reorg::record(conn, chain_id, from_block).await?;
                Ok(deleted)
            }
            .scope_boxed()
        })
        .await
        .map_err(IngesterError::from)
    }
}
