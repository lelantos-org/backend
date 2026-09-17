//! `gov_votes`: one row per vote cast on the governor.

use crate::domain::error::ProtocolIndexerError;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::gov_votes;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = gov_votes)]
pub struct NewGovVote {
    pub chain_id: i64,
    pub proposal_id: BigDecimal,
    pub voter: Vec<u8>,
    pub support: i16,
    pub weight: BigDecimal,
    pub reason: String,
    pub params: Option<Vec<u8>>,
    pub block_number: i64,
    pub log_index: i32,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}

/// Insert a whole tick's votes in one statement.
///
/// `ON CONFLICT DO NOTHING` on `(chain_id, proposal_id, voter)`: the governor
/// accepts one vote per account per proposal, so a conflict is only a replay.
///
/// Inserted whether or not the proposal is in `gov_proposals`. A governor that
/// predates the ingester's start block has votes on proposals never indexed;
/// keeping them costs nothing and fails no tick, and no route serves votes for
/// a proposal it cannot find.
pub async fn insert_batch(
    pool: &DbPool,
    rows: &[NewGovVote],
) -> Result<usize, ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut conn = super::conn(pool).await?;
    Ok(diesel::insert_into(gov_votes::table)
        .values(rows)
        .on_conflict((
            gov_votes::chain_id,
            gov_votes::proposal_id,
            gov_votes::voter,
        ))
        .do_nothing()
        .execute(&mut conn)
        .await?)
}
