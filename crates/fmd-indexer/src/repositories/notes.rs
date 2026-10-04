use crate::domain::error::Result;
use async_trait::async_trait;
use database::DbPool;
use database::listen::{self, CHANNEL_NOTES_APPENDED};
pub use database::models::{LeafRow, NewNote, NoteRow};
use database::schema::notes;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[async_trait]
pub trait NotesRepo: Send + Sync {
    /// Insert notes, skipping any whose `(chain_id, leaf_index)` is already
    /// stored, and return how many were written.
    ///
    /// A note is identified by its leaf position, not its commitment: two
    /// leaves may hold the same `cm`, and both are rows.
    async fn insert_batch(&self, rows: &[NewNote]) -> Result<usize>;
    async fn delete_from_block(&self, chain_id: i64, from_block: i64) -> Result<usize>;
    async fn fetch_after(&self, chain_id: i64, after_id: i64, limit: i64) -> Result<Vec<NoteRow>>;

    /// Chain-agnostic variant for the subscription backfill pass, which tracks a
    /// single global `notes.id` pointer rather than a per-chain cursor.
    async fn fetch_after_any_chain(&self, after_id: i64, limit: i64) -> Result<Vec<NoteRow>>;

    /// Highest ingested `notes.id` across all chains, or 0 when empty.
    async fn max_id(&self) -> Result<i64>;

    /// Leaves of `chain_id` in `[from, to)`, ordered by `leaf_index`.
    ///
    /// Only the one-shot tree backfill reads this, so it is paged rather than
    /// streamed: a chain with millions of notes would otherwise materialise every
    /// leaf at once.
    async fn leaves(&self, chain_id: i64, from: i64, to: i64) -> Result<Vec<LeafRow>>;

    /// Wake the filter loop after a commit.
    ///
    /// Infallible by contract: the notes are already committed on connections
    /// this call cannot roll back, and the filter's cursor finds them on its next
    /// poll regardless, so a failed wake is logged rather than surfaced.
    async fn notify_appended(&self, chain_id: i64);
}

/// Rows per INSERT. Postgres caps a statement at 65535 bind parameters and
/// `NewNote` binds 11 columns, so chunking keeps a large `filter_batch` from
/// failing every tick.
const INSERT_CHUNK: usize = 2000;

pub struct PostgresNotesRepo {
    pool: DbPool,
}

impl PostgresNotesRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl NotesRepo for PostgresNotesRepo {
    async fn insert_batch(&self, rows: &[NewNote]) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut conn = super::conn(&self.pool).await?;
        let mut n = 0;
        for chunk in rows.chunks(INSERT_CHUNK) {
            n += diesel::insert_into(notes::table)
                .values(chunk)
                .on_conflict((notes::chain_id, notes::leaf_index))
                .do_nothing()
                .execute(&mut conn)
                .await?;
        }
        Ok(n)
    }

    async fn delete_from_block(&self, chain_id: i64, from_block: i64) -> Result<usize> {
        let mut conn = super::conn(&self.pool).await?;
        let n = diesel::delete(
            notes::table
                .filter(notes::chain_id.eq(chain_id))
                .filter(notes::block_number.ge(from_block)),
        )
        .execute(&mut conn)
        .await?;
        Ok(n)
    }

    async fn fetch_after(&self, chain_id: i64, after_id: i64, limit: i64) -> Result<Vec<NoteRow>> {
        let mut conn = super::conn(&self.pool).await?;
        let rows = notes::table
            .filter(notes::chain_id.eq(chain_id))
            .filter(notes::id.gt(after_id))
            .order(notes::id.asc())
            .limit(limit)
            .select(NoteRow::as_select())
            .load(&mut conn)
            .await?;
        Ok(rows)
    }

    async fn fetch_after_any_chain(&self, after_id: i64, limit: i64) -> Result<Vec<NoteRow>> {
        let mut conn = super::conn(&self.pool).await?;
        let rows = notes::table
            .filter(notes::id.gt(after_id))
            .order(notes::id.asc())
            .limit(limit)
            .select(NoteRow::as_select())
            .load(&mut conn)
            .await?;
        Ok(rows)
    }

    async fn max_id(&self) -> Result<i64> {
        let mut conn = super::conn(&self.pool).await?;
        let max: Option<i64> = notes::table
            .select(diesel::dsl::max(notes::id))
            .first(&mut conn)
            .await?;
        Ok(max.unwrap_or(0))
    }

    async fn leaves(&self, chain_id: i64, from: i64, to: i64) -> Result<Vec<LeafRow>> {
        let mut conn = super::conn(&self.pool).await?;
        Ok(notes::table
            .filter(notes::chain_id.eq(chain_id))
            .filter(notes::leaf_index.ge(from))
            .filter(notes::leaf_index.lt(to))
            .order(notes::leaf_index.asc())
            .select(LeafRow::as_select())
            .load(&mut conn)
            .await?)
    }

    async fn notify_appended(&self, chain_id: i64) {
        listen::notify_best_effort(&self.pool, CHANNEL_NOTES_APPENDED, &chain_id.to_string()).await;
    }
}
