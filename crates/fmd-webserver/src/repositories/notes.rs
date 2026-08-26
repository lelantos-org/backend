use crate::domain::error::{AppError, AppResult};
use database::DbPool;
pub use database::models::{LeafInputsRow, NoteRow};
use database::schema::notes;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Above this the planner's row estimate stands in for an exact count.
///
/// The only consumer buckets the value by `log2`, so a few percent of drift
/// cannot move the answer once the table is this size. Below it the exact count
/// is a small scan, and the estimate is the less trustworthy of the two: it is
/// `-1` until the table is first analysed and lags every bulk insert after that.
const ESTIMATE_FLOOR: i64 = 100_000;

#[derive(QueryableByName)]
struct Reltuples {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    estimate: i64,
}

/// Total notes across all chains: the pool a subscription's false positives are
/// drawn from, which bounds how precise a γ may be.
///
/// Answers from `pg_class.reltuples` where that is large enough to be both
/// trustworthy and worth having, and falls back to `COUNT(*)` otherwise. A stale
/// or missing estimate therefore costs the scan it always cost, never a wrong
/// bound.
pub async fn count_all(pool: &DbPool) -> AppResult<i64> {
    let mut conn = super::conn(pool).await?;
    let estimate: i64 = diesel::sql_query(
        "SELECT reltuples::BIGINT AS estimate FROM pg_class WHERE oid = 'notes'::regclass",
    )
    .get_result::<Reltuples>(&mut conn)
    .await
    .map(|r| r.estimate)
    .map_err(|e| AppError::Db(e.to_string()))?;

    if estimate >= ESTIMATE_FLOOR {
        return Ok(estimate);
    }
    notes::table
        .count()
        .get_result(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}

/// Highest `notes.id` for `chain_id`, or 0 when the chain has none.
///
/// Indexed `MAX()` on the primary key, so this is an index scan rather than the
/// full scan `count_all` pays for, and is safe to poll far more often than the
/// pages it gates.
pub async fn max_id(pool: &DbPool, chain_id: i64) -> AppResult<i64> {
    let mut conn = super::conn(pool).await?;
    let max: Option<i64> = notes::table
        .filter(notes::chain_id.eq(chain_id))
        .select(diesel::dsl::max(notes::id))
        .first(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))?;
    Ok(max.unwrap_or(0))
}

pub async fn list(
    pool: &DbPool,
    chain_id: Option<i64>,
    after_id: i64,
    limit: i64,
) -> AppResult<Vec<NoteRow>> {
    let mut conn = super::conn(pool).await?;
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
/// Index-only lookup on `notes_chain_leaf_idx`, so it stays O(log n) and can be
/// called per request. The tree mirror uses it to detect a reorg that has trimmed
/// leaves beneath it.
pub async fn max_leaf_index(pool: &DbPool, chain_id: i64) -> AppResult<Option<i64>> {
    let mut conn = super::conn(pool).await?;
    notes::table
        .filter(notes::chain_id.eq(chain_id))
        .select(diesel::dsl::max(notes::leaf_index))
        .first(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}

/// `(leaf_index, cm, cv_dep)` for a chain, ordered by `leaf_index`, from `from`
/// up to but excluding `to`. Source for the in-memory tree mirror and for the
/// commitment-chunk endpoint.
///
/// `from` lets the mirror append only what it has not already hashed; re-hashing
/// every leaf on each rebuild is the dominant cost of serving tree state. `to`
/// is `None` for the mirror, which wants everything above its own head.
pub async fn list_leaf_inputs(
    pool: &DbPool,
    chain_id: i64,
    from: i64,
    to: Option<i64>,
) -> AppResult<Vec<LeafInputsRow>> {
    let mut conn = super::conn(pool).await?;
    let mut q = notes::table
        .filter(notes::chain_id.eq(chain_id))
        .filter(notes::leaf_index.ge(from))
        .into_boxed();
    if let Some(to) = to {
        q = q.filter(notes::leaf_index.lt(to));
    }
    q.order(notes::leaf_index.asc())
        .select(LeafInputsRow::as_select())
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
