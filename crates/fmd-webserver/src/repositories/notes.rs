use crate::domain::error::{AppError, AppResult};
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::notes;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

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

/// Total notes across all chains — the pool a subscription's false positives
/// are drawn from, so it bounds how precise a γ may be.
pub async fn count_all(pool: &DbPool) -> AppResult<i64> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    notes::table
        .count()
        .get_result(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}

#[derive(Debug, Clone)]
pub struct LeafInputsRow {
    pub leaf_index: i64,
    pub cm: Vec<u8>,
    pub cv_dep_x: BigDecimal,
    pub cv_dep_y: BigDecimal,
}

pub async fn list(
    pool: &DbPool,
    chain_id: Option<i64>,
    after_id: i64,
    limit: i64,
) -> AppResult<Vec<NoteRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    let mut q = notes::table.into_boxed();
    if let Some(c) = chain_id {
        q = q.filter(notes::chain_id.eq(c));
    }
    q.filter(notes::id.gt(after_id))
        .order(notes::id.asc())
        .limit(limit)
        .select(NoteRow::as_select())
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}

/// Highest `leaf_index` for a chain, or `None` when the chain has no notes.
///
/// Index-only lookup on `notes_chain_leaf_idx`, so it stays O(log n) and can
/// be called per request. The tree mirror uses it to notice that a reorg has
/// trimmed leaves out from under it.
pub async fn max_leaf_index(pool: &DbPool, chain_id: i64) -> AppResult<Option<i64>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    notes::table
        .filter(notes::chain_id.eq(chain_id))
        .select(diesel::dsl::max(notes::leaf_index))
        .first(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}

/// `(leaf_index, cm, cv_dep)` tuples for a chain with `leaf_index >= from`,
/// ordered by leaf_index. Source for the in-memory tree mirror; cv_dep is
/// needed because `leaf = Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`.
///
/// `from` lets the mirror append only what it has not already hashed —
/// re-hashing every leaf on each rebuild is the dominant cost of serving
/// tree state.
pub async fn list_leaf_inputs_from(
    pool: &DbPool,
    chain_id: i64,
    from: i64,
) -> AppResult<Vec<LeafInputsRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    let rows: Vec<(i64, Vec<u8>, BigDecimal, BigDecimal)> = notes::table
        .filter(notes::chain_id.eq(chain_id))
        .filter(notes::leaf_index.ge(from))
        .order(notes::leaf_index.asc())
        .select((
            notes::leaf_index,
            notes::cm,
            notes::cv_dep_x,
            notes::cv_dep_y,
        ))
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|(leaf_index, cm, cv_dep_x, cv_dep_y)| LeafInputsRow {
            leaf_index,
            cm,
            cv_dep_x,
            cv_dep_y,
        })
        .collect())
}

pub struct CommitmentChunkEntry {
    pub leaf_index: i64,
    pub cm: Vec<u8>,
    pub cv_dep_x: BigDecimal,
    pub cv_dep_y: BigDecimal,
}

pub async fn list_chunk(
    pool: &DbPool,
    chain_id: i64,
    from_leaf: i64,
    to_leaf: i64,
) -> AppResult<Vec<CommitmentChunkEntry>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    let rows: Vec<(i64, Vec<u8>, BigDecimal, BigDecimal)> = notes::table
        .filter(notes::chain_id.eq(chain_id))
        .filter(notes::leaf_index.ge(from_leaf))
        .filter(notes::leaf_index.lt(to_leaf))
        .order(notes::leaf_index.asc())
        .select((
            notes::leaf_index,
            notes::cm,
            notes::cv_dep_x,
            notes::cv_dep_y,
        ))
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(leaf_index, cm, cv_dep_x, cv_dep_y)| CommitmentChunkEntry {
                leaf_index,
                cm,
                cv_dep_x,
                cv_dep_y,
            },
        )
        .collect())
}
