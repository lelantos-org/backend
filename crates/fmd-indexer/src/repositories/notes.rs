use crate::domain::error::{FmdIndexerError, Result};
use async_trait::async_trait;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::notes;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = notes)]
pub struct NewNote {
    pub chain_id: i64,
    pub block_number: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub cm: Vec<u8>,
    pub clue_rx: BigDecimal,
    pub clue_ry: BigDecimal,
    pub eph_pub_x: BigDecimal,
    pub eph_pub_y: BigDecimal,
    pub ciphertext: Vec<u8>,
    pub leaf_index: i64,
    pub cv_dep_x: BigDecimal,
    pub cv_dep_y: BigDecimal,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = notes)]
pub struct NoteRow {
    pub id: i64,
    pub chain_id: i64,
    pub block_number: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub cm: Vec<u8>,
    pub clue_rx: BigDecimal,
    pub clue_ry: BigDecimal,
    pub eph_pub_x: BigDecimal,
    pub eph_pub_y: BigDecimal,
    pub ciphertext: Vec<u8>,
    pub leaf_index: i64,
    pub cv_dep_x: BigDecimal,
    pub cv_dep_y: BigDecimal,
}

#[async_trait]
pub trait NotesRepo: Send + Sync {
    async fn insert_batch(&self, rows: &[NewNote]) -> Result<usize>;
    async fn delete_from_block(&self, chain_id: i64, from_block: i64) -> Result<usize>;
    async fn fetch_after(&self, chain_id: i64, after_id: i64, limit: i64) -> Result<Vec<NoteRow>>;

    /// Chain-agnostic variant for the subscription backfill pass, which
    /// tracks a single global `notes.id` pointer rather than a per-chain
    /// cursor.
    async fn fetch_after_any_chain(&self, after_id: i64, limit: i64) -> Result<Vec<NoteRow>>;

    /// Highest ingested `notes.id` across all chains, or 0 when empty.
    async fn max_id(&self) -> Result<i64>;
}

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
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        let n = diesel::insert_into(notes::table)
            .values(rows)
            .on_conflict((notes::chain_id, notes::cm))
            .do_nothing()
            .execute(&mut conn)
            .await?;
        Ok(n)
    }

    async fn delete_from_block(&self, chain_id: i64, from_block: i64) -> Result<usize> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
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
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
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
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
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
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        let max: Option<i64> = notes::table
            .select(diesel::dsl::max(notes::id))
            .first(&mut conn)
            .await?;
        Ok(max.unwrap_or(0))
    }
}
