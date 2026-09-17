//! Reads of `gov_votes`, which protocol-indexer writes.

use super::Position;
use crate::domain::error::AppResult;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::gov_votes;
use diesel::prelude::*;
use diesel::sql_types::{Array, BigInt, Numeric, SmallInt};
use diesel_async::RunQueryDsl;

/// Summed weight and vote count for one side of one proposal.
#[derive(Debug, Clone, QueryableByName)]
pub struct TallyRow {
    #[diesel(sql_type = Numeric)]
    pub proposal_id: BigDecimal,
    /// 0 Against, 1 For, 2 Abstain.
    #[diesel(sql_type = SmallInt)]
    pub support: i16,
    #[diesel(sql_type = Numeric)]
    pub weight: BigDecimal,
    #[diesel(sql_type = BigInt)]
    pub votes: i64,
}

/// Tallies for every proposal in `proposal_ids`, in one statement.
///
/// Summed in the database rather than by loading votes: a proposal can carry
/// thousands of them and a page shows only three numbers each. A side with no
/// votes has no row.
pub async fn tallies(
    pool: &DbPool,
    chain_id: i64,
    proposal_ids: &[BigDecimal],
) -> AppResult<Vec<TallyRow>> {
    if proposal_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = super::conn(pool).await?;
    diesel::sql_query(
        "SELECT proposal_id, support, SUM(weight) AS weight, COUNT(*) AS votes \
           FROM gov_votes \
          WHERE chain_id = $1 AND proposal_id = ANY($2) \
          GROUP BY proposal_id, support",
    )
    .bind::<BigInt, _>(chain_id)
    .bind::<Array<Numeric>, _>(proposal_ids)
    .load(&mut conn)
    .await
    .map_err(super::db_err)
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = gov_votes)]
pub struct VoteRow {
    pub voter: Vec<u8>,
    pub support: i16,
    pub weight: BigDecimal,
    pub reason: String,
    pub block_number: i64,
    pub log_index: i32,
    pub tx_hash: Vec<u8>,
}

/// Up to `limit` votes on one proposal strictly older than `before`.
pub async fn list(
    pool: &DbPool,
    chain_id: i64,
    proposal_id: &BigDecimal,
    before: Option<Position>,
    limit: i64,
) -> AppResult<Vec<VoteRow>> {
    use gov_votes as v;
    let mut conn = super::conn(pool).await?;
    let mut q = v::table
        .filter(v::chain_id.eq(chain_id))
        .filter(v::proposal_id.eq(proposal_id))
        .select(VoteRow::as_select())
        .order((v::block_number.desc(), v::log_index.desc()))
        .limit(limit)
        .into_boxed();
    if let Some((block, log)) = before {
        q = q.filter(
            v::block_number
                .lt(block)
                .or(v::block_number.eq(block).and(v::log_index.lt(log))),
        );
    }
    q.load(&mut conn).await.map_err(super::db_err)
}
