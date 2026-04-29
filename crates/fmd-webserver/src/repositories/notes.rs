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

/// All (leaf_index, cm, cv_dep) tuples for a chain ordered by leaf_index.
/// Source for the in-memory tree mirror; cv_dep is needed because
/// `leaf = Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`.
pub async fn list_leaf_inputs_by_leaf(
    pool: &DbPool,
    chain_id: i64,
) -> AppResult<Vec<LeafInputsRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    let rows: Vec<(i64, Vec<u8>, BigDecimal, BigDecimal)> = notes::table
        .filter(notes::chain_id.eq(chain_id))
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

pub async fn find_leaf_inputs_by_cm(
    pool: &DbPool,
    chain_id: i64,
    cm: &[u8],
) -> AppResult<Option<LeafInputsRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    let row: Option<(i64, BigDecimal, BigDecimal)> = notes::table
        .filter(notes::chain_id.eq(chain_id))
        .filter(notes::cm.eq(cm))
        .select((notes::leaf_index, notes::cv_dep_x, notes::cv_dep_y))
        .first(&mut conn)
        .await
        .optional()
        .map_err(|e| AppError::Db(e.to_string()))?;
    Ok(row.map(|(leaf_index, cv_dep_x, cv_dep_y)| LeafInputsRow {
        leaf_index,
        cm: cm.to_vec(),
        cv_dep_x,
        cv_dep_y,
    }))
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
        .select((notes::leaf_index, notes::cm, notes::cv_dep_x, notes::cv_dep_y))
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|(leaf_index, cm, cv_dep_x, cv_dep_y)| CommitmentChunkEntry {
            leaf_index,
            cm,
            cv_dep_x,
            cv_dep_y,
        })
        .collect())
}

pub async fn find_leaf_index_by_cm(
    pool: &DbPool,
    chain_id: i64,
    cm: &[u8],
) -> AppResult<Option<i64>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    notes::table
        .filter(notes::chain_id.eq(chain_id))
        .filter(notes::cm.eq(cm))
        .select(notes::leaf_index)
        .first::<i64>(&mut conn)
        .await
        .optional()
        .map_err(|e| AppError::Db(e.to_string()))
}
