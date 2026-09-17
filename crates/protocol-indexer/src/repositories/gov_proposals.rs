//! `gov_proposals`: one row per governor proposal, plus its lifecycle marks.

use crate::domain::error::ProtocolIndexerError;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::gov_proposals;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = gov_proposals)]
pub struct NewGovProposal {
    pub chain_id: i64,
    pub proposal_id: BigDecimal,
    pub proposer: Vec<u8>,
    pub targets: Vec<Vec<u8>>,
    pub call_values: Vec<BigDecimal>,
    pub signatures: Vec<String>,
    pub calldatas: Vec<Vec<u8>>,
    pub description: String,
    pub vote_start: i64,
    pub vote_end: i64,
    pub block_number: i64,
    pub log_index: i32,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}

/// Insert a whole tick's proposals in one statement.
///
/// `ON CONFLICT DO NOTHING` on the primary key, so a replayed window converges.
/// The governor refuses to re-propose an existing id, so a conflict is only ever
/// a replay.
pub async fn insert_batch(
    pool: &DbPool,
    rows: &[NewGovProposal],
) -> Result<usize, ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut conn = super::conn(pool).await?;
    Ok(diesel::insert_into(gov_proposals::table)
        .values(rows)
        .on_conflict((gov_proposals::chain_id, gov_proposals::proposal_id))
        .do_nothing()
        .execute(&mut conn)
        .await?)
}

/// Which lifecycle column a [`mark_batch`] call writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    QuorumVoteDeadline,
    Queued,
    Executed,
    Canceled,
}

/// One lifecycle event, as the `UPDATE` recording it needs it.
#[derive(Debug, Clone)]
pub struct MarkProposal {
    pub chain_id: i64,
    pub proposal_id: BigDecimal,
    /// The block the event was emitted in.
    pub block_number: i64,
    /// The quorum vote deadline for [`Mark::QuorumVoteDeadline`], the ETA for
    /// [`Mark::Queued`], and unused otherwise.
    pub value: i64,
}

/// Apply one kind of mark to a tick's proposals over one pooled connection.
///
/// One `UPDATE` per row, as `deposit_escrowed_events::mark_flushed_batch` does:
/// each names a different proposal. A mark for a proposal this table never saw
/// — created below the ingester's start block — matches nothing and is dropped,
/// since there is no row to hang it on.
pub async fn mark_batch(
    pool: &DbPool,
    mark: Mark,
    rows: &[MarkProposal],
) -> Result<(), ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = super::conn(pool).await?;
    for row in rows {
        let target = gov_proposals::table
            .filter(gov_proposals::chain_id.eq(row.chain_id))
            .filter(gov_proposals::proposal_id.eq(row.proposal_id.clone()));
        let update = diesel::update(target);
        match mark {
            Mark::QuorumVoteDeadline => {
                update
                    .set(gov_proposals::quorum_vote_deadline.eq(Some(row.value)))
                    .execute(&mut conn)
                    .await?
            }
            Mark::Queued => {
                update
                    .set((
                        gov_proposals::queued_at_block.eq(Some(row.block_number)),
                        gov_proposals::eta.eq(Some(row.value)),
                    ))
                    .execute(&mut conn)
                    .await?
            }
            Mark::Executed => {
                update
                    .set(gov_proposals::executed_at_block.eq(Some(row.block_number)))
                    .execute(&mut conn)
                    .await?
            }
            Mark::Canceled => {
                update
                    .set(gov_proposals::canceled_at_block.eq(Some(row.block_number)))
                    .execute(&mut conn)
                    .await?
            }
        };
    }
    Ok(())
}
