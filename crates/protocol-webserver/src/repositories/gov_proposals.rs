//! Reads of `gov_proposals`, which protocol-indexer writes.

use super::Position;
use crate::domain::error::AppResult;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::gov_proposals;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = gov_proposals)]
pub struct ProposalRow {
    pub proposal_id: BigDecimal,
    pub proposer: Vec<u8>,
    pub targets: Vec<Vec<u8>>,
    pub call_values: Vec<BigDecimal>,
    pub signatures: Vec<String>,
    pub calldatas: Vec<Vec<u8>>,
    pub description: String,
    pub vote_start: i64,
    pub vote_end: i64,
    pub quorum_vote_deadline: Option<i64>,
    pub block_number: i64,
    pub log_index: i32,
    pub tx_hash: Vec<u8>,
    pub queued_at_block: Option<i64>,
    pub eta: Option<i64>,
    pub executed_at_block: Option<i64>,
    pub canceled_at_block: Option<i64>,
}

/// Up to `limit` proposals on `chain_id` strictly older than `before`.
pub async fn list(
    pool: &DbPool,
    chain_id: i64,
    before: Option<Position>,
    limit: i64,
) -> AppResult<Vec<ProposalRow>> {
    use gov_proposals as g;
    let mut conn = super::conn(pool).await?;
    let mut q = g::table
        .filter(g::chain_id.eq(chain_id))
        .select(ProposalRow::as_select())
        .order((g::block_number.desc(), g::log_index.desc()))
        .limit(limit)
        .into_boxed();
    if let Some((block, log)) = before {
        q = q.filter(
            g::block_number
                .lt(block)
                .or(g::block_number.eq(block).and(g::log_index.lt(log))),
        );
    }
    q.load(&mut conn).await.map_err(super::db_err)
}

pub async fn get(
    pool: &DbPool,
    chain_id: i64,
    proposal_id: &BigDecimal,
) -> AppResult<Option<ProposalRow>> {
    use gov_proposals as g;
    let mut conn = super::conn(pool).await?;
    g::table
        .filter(g::chain_id.eq(chain_id))
        .filter(g::proposal_id.eq(proposal_id))
        .select(ProposalRow::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(super::db_err)
}
