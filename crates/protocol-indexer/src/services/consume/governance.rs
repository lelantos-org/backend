//! The governor's ledger: `ProposalCreated`, `ProposalQuorumVoteDeadline`,
//! `VoteCast`/`VoteCastWithParams`, `ProposalQueued`, `ProposalExecuted` and
//! `ProposalCanceled` into `gov_proposals` and `gov_votes`.
//!
//! These are the only events this crate reads that the pool does not emit, and
//! OpenZeppelin's signatures are generic enough (`ProposalExecuted(uint256)`)
//! that any contract could emit one. So unlike the pool's events they are
//! accepted only from the governor configured for the chain; see
//! [`accepts_emitter`].
//!
//! Pure up to [`GovernancePlan::apply`] — the push methods build rows and touch
//! no database.

use crate::domain::address;
use crate::domain::error::ProtocolIndexerError;
use crate::repositories::gov_proposals::{self, Mark, MarkProposal, NewGovProposal};
use crate::repositories::gov_votes::{self, NewGovVote};
use alloy::primitives::{Address, U256};
use chain_types::numeric::u256_to_bigdecimal;
use database::{DbPool, RawEventRow};
use shared::entities::EventKind;

/// Whether `kind` is emitted by the governor rather than the pool.
fn is_governance(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::ProposalCreated
            | EventKind::ProposalQuorumVoteDeadline
            | EventKind::VoteCast
            | EventKind::VoteCastWithParams
            | EventKind::ProposalQueued
            | EventKind::ProposalExecuted
            | EventKind::ProposalCanceled
    )
}

/// Whether `row`, of `kind`, came from a contract this crate accepts it from.
///
/// The pool's events are accepted on topic0 alone. A governance event only when
/// `governor` emitted it: `None` — no governor configured for the chain —
/// accepts none, and so does a row with no recorded emitter, since the column
/// predates nothing the governor ever emitted.
pub fn accepts_emitter(kind: EventKind, governor: Option<Address>, row: &RawEventRow) -> bool {
    if !is_governance(kind) {
        return true;
    }
    let emitter = row.address.as_deref().and_then(address::from_column);
    governor.is_some() && emitter == governor
}

/// A governor clock value (unix seconds for `LelantosToken`) as a column.
///
/// Saturates rather than failing: a value past `i64::MAX` seconds is not a date
/// any client renders, and refusing it would wedge the chain on one log.
fn clock(v: U256) -> i64 {
    u64::try_from(v)
        .ok()
        .and_then(|x| i64::try_from(x).ok())
        .unwrap_or(i64::MAX)
}

/// One window's writes to `gov_proposals` and `gov_votes`.
#[derive(Debug, Default)]
pub struct GovernancePlan {
    pub proposals: Vec<NewGovProposal>,
    pub quorum_vote_deadlines: Vec<MarkProposal>,
    pub votes: Vec<NewGovVote>,
    pub queued: Vec<MarkProposal>,
    pub executed: Vec<MarkProposal>,
    pub canceled: Vec<MarkProposal>,
}

impl GovernancePlan {
    #[allow(clippy::too_many_arguments)]
    pub fn push_created(
        &mut self,
        chain_id: i64,
        row: &RawEventRow,
        proposal_id: U256,
        proposer: Address,
        targets: Vec<Address>,
        values: Vec<U256>,
        signatures: Vec<String>,
        calldatas: Vec<Vec<u8>>,
        vote_start: U256,
        vote_end: U256,
        description: String,
    ) {
        self.proposals.push(NewGovProposal {
            chain_id,
            proposal_id: u256_to_bigdecimal(proposal_id),
            proposer: proposer.to_vec(),
            targets: targets.into_iter().map(|t| t.to_vec()).collect(),
            call_values: values.into_iter().map(u256_to_bigdecimal).collect(),
            signatures,
            calldatas,
            description,
            vote_start: clock(vote_start),
            vote_end: clock(vote_end),
            block_number: row.block_number,
            log_index: row.log_index,
            tx_hash: row.tx_hash.clone(),
            block_ts: row.block_ts,
        });
    }

    pub fn push_quorum_vote_deadline(
        &mut self,
        chain_id: i64,
        row: &RawEventRow,
        proposal_id: U256,
        quorum_vote_deadline: U256,
    ) {
        self.quorum_vote_deadlines.push(mark(
            chain_id,
            row,
            proposal_id,
            clock(quorum_vote_deadline),
        ));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push_vote(
        &mut self,
        chain_id: i64,
        row: &RawEventRow,
        voter: Address,
        proposal_id: U256,
        support: u8,
        weight: U256,
        reason: String,
        params: Option<Vec<u8>>,
    ) {
        self.votes.push(NewGovVote {
            chain_id,
            proposal_id: u256_to_bigdecimal(proposal_id),
            voter: voter.to_vec(),
            support: i16::from(support),
            weight: u256_to_bigdecimal(weight),
            reason,
            params,
            block_number: row.block_number,
            log_index: row.log_index,
            tx_hash: row.tx_hash.clone(),
            block_ts: row.block_ts,
        });
    }

    pub fn push_queued(&mut self, chain_id: i64, row: &RawEventRow, proposal_id: U256, eta: U256) {
        self.queued
            .push(mark(chain_id, row, proposal_id, clock(eta)));
    }

    pub fn push_executed(&mut self, chain_id: i64, row: &RawEventRow, proposal_id: U256) {
        self.executed.push(mark(chain_id, row, proposal_id, 0));
    }

    pub fn push_canceled(&mut self, chain_id: i64, row: &RawEventRow, proposal_id: U256) {
        self.canceled.push(mark(chain_id, row, proposal_id, 0));
    }

    /// Proposals first, since every other write is keyed on the row that insert
    /// creates: `ProposalQuorumVoteDeadline` is emitted in the creating transaction
    /// and so routinely shares its window. Votes next, then the marks in the
    /// order a proposal's life takes them.
    ///
    /// Votes do not depend on the proposal row — `gov_votes` has no foreign key —
    /// but keeping them after it keeps the causal order readable.
    pub async fn apply(&self, pool: &DbPool) -> Result<(), ProtocolIndexerError> {
        gov_proposals::insert_batch(pool, &self.proposals).await?;
        gov_proposals::mark_batch(pool, Mark::QuorumVoteDeadline, &self.quorum_vote_deadlines)
            .await?;
        gov_votes::insert_batch(pool, &self.votes).await?;
        gov_proposals::mark_batch(pool, Mark::Queued, &self.queued).await?;
        gov_proposals::mark_batch(pool, Mark::Executed, &self.executed).await?;
        gov_proposals::mark_batch(pool, Mark::Canceled, &self.canceled).await?;
        Ok(())
    }
}

fn mark(chain_id: i64, row: &RawEventRow, proposal_id: U256, value: i64) -> MarkProposal {
    MarkProposal {
        chain_id,
        proposal_id: u256_to_bigdecimal(proposal_id),
        block_number: row.block_number,
        value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(address: Option<Vec<u8>>) -> RawEventRow {
        RawEventRow {
            id: 1,
            chain_id: 1,
            block_number: 1,
            evm_block_number: None,
            block_hash: vec![],
            block_ts: 0,
            tx_hash: vec![],
            log_index: 0,
            event_kind: EventKind::ProposalCreated.as_i16(),
            topics: vec![],
            data: vec![],
            address,
        }
    }

    const CREATED: EventKind = EventKind::ProposalCreated;

    #[test]
    fn only_the_configured_governor_is_accepted() {
        let gov = Address::repeat_byte(0x11);
        assert!(accepts_emitter(
            CREATED,
            Some(gov),
            &row(Some(gov.to_vec()))
        ));
        assert!(!accepts_emitter(
            CREATED,
            Some(gov),
            &row(Some(Address::repeat_byte(0x22).to_vec()))
        ));
    }

    /// No governor configured means governance is off for the chain, not that
    /// any emitter will do.
    #[test]
    fn no_governor_and_no_emitter_accept_nothing() {
        let gov = Address::repeat_byte(0x11);
        assert!(!accepts_emitter(CREATED, None, &row(Some(gov.to_vec()))));
        assert!(!accepts_emitter(CREATED, Some(gov), &row(None)));
    }

    /// The pool's events carry no emitter check.
    #[test]
    fn pool_events_are_accepted_from_any_emitter() {
        assert!(accepts_emitter(
            EventKind::DepositEscrowed,
            None,
            &row(None)
        ));
    }

    #[test]
    fn governance_kinds_are_exactly_the_governor_events() {
        let gov: Vec<_> = EventKind::ALL
            .into_iter()
            .filter(|k| is_governance(*k))
            .collect();
        assert_eq!(gov.len(), 7);
        assert!(!is_governance(EventKind::DepositEscrowed));
    }

    #[test]
    fn clock_saturates_instead_of_wrapping() {
        assert_eq!(clock(U256::from(1_700_000_000u64)), 1_700_000_000);
        assert_eq!(clock(U256::MAX), i64::MAX);
        assert_eq!(clock(U256::from(u64::MAX)), i64::MAX);
    }
}
