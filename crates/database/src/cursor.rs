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
    /// Unconditional write. Last writer wins, including backwards — use only
    /// where a rewind is the intent (cursor reset). Prefer
    /// [`CursorRepo::upsert_monotonic`] for normal batch advances.
    async fn upsert(&self, row: UpsertCursor) -> CursorResult<()>;
    /// Advance only. A write whose `last_event_id` is not greater than the
    /// stored one is a no-op.
    ///
    /// Guards the read-modify-write in every consume/filter tick: two
    /// processes that fetched the same cursor can otherwise have the slower
    /// one overwrite the faster one's watermark, dragging the cursor backwards
    /// and re-processing an unbounded range.
    ///
    /// Returns whether the row was written. `false` means a peer's watermark
    /// was already at or past this one — benign where concurrent writers are
    /// expected (the filter loop), and a split brain where they are not (the
    /// consume loop holds a per-chain advisory lock precisely so that one
    /// writer exists).
    async fn upsert_monotonic(&self, row: UpsertCursor) -> CursorResult<bool>;
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

/// `INSERT … ON CONFLICT … DO UPDATE SET … WHERE last_event_id < $new`.
///
/// Split out so the emitted SQL can be asserted in a unit test — diesel puts
/// the predicate in the `DO UPDATE` clause rather than on the conflict target,
/// and that distinction is the whole point of the method.
fn monotonic_stmt(
    row: &UpsertCursor,
) -> impl diesel::query_builder::QueryFragment<diesel::pg::Pg> + diesel::query_builder::QueryId + use<'_>
{
    use diesel::query_dsl::methods::FilterDsl;
    FilterDsl::filter(
        diesel::insert_into(consumer_cursors::table)
            .values(row)
            .on_conflict((consumer_cursors::name, consumer_cursors::chain_id))
            .do_update()
            .set(row),
        consumer_cursors::last_event_id.lt(row.last_event_id),
    )
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

    async fn upsert_monotonic(&self, row: UpsertCursor) -> CursorResult<bool> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| CursorError::Pool(e.to_string()))?;
        // The `WHERE last_event_id < $new` predicate lives in the `DO UPDATE`
        // clause, so a rejected advance is reported as zero affected rows
        // rather than an error.
        Ok(monotonic_stmt(&row).execute(&mut conn).await? > 0)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the clause the predicate lands in. Diesel can attach a `filter` to
    /// the conflict *target* (a partial-index predicate) instead of the
    /// `DO UPDATE`; that would silently make the guard a no-op.
    #[test]
    fn monotonic_guard_is_on_do_update() {
        let row = UpsertCursor {
            name: "fmd".into(),
            chain_id: 1,
            last_event_id: 42,
            last_block_number: 7,
        };
        let sql = diesel::debug_query::<diesel::pg::Pg, _>(&monotonic_stmt(&row)).to_string();
        let update_at = sql.find("DO UPDATE").expect("DO UPDATE clause");
        let where_at = sql.find("WHERE").expect("WHERE clause");
        assert!(
            where_at > update_at,
            "WHERE must qualify DO UPDATE, not the conflict target: {sql}"
        );
        assert!(
            sql.contains(r#""consumer_cursors"."last_event_id" <"#),
            "guard must compare the stored cursor: {sql}"
        );
    }
}
