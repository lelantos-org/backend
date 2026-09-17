//! The governor's proposals and votes, as `/v1/governance/*` publishes them.
//!
//! uint256 values are decimal strings, timestamps and block numbers JSON
//! numbers, addresses EIP-55 and bytes `0x` hex.
//!
//! No proposal *state* (Pending, Active, Defeated, …): that depends on the clock
//! and on quorum at the snapshot, so a client reads `state()` from the governor
//! rather than trusting a value this service computed at some earlier instant.

use serde::Serialize;
use utoipa::ToSchema;

/// Summed vote weight per side, from the votes indexed so far.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct TalliesOut {
    #[serde(rename = "for")]
    pub for_votes: String,
    pub against: String,
    pub abstain: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProposalSummaryOut {
    /// Decimal uint256.
    pub proposal_id: String,
    pub proposer: String,
    /// The description's first non-empty line, leading `#`s and whitespace
    /// stripped, at most 200 characters.
    pub title: String,
    /// Unix seconds (the governor's timestamp clock).
    pub vote_start: i64,
    pub vote_end: i64,
    /// After this instant only Against votes are accepted. Absent if the
    /// governor's `ProposalQuorumVoteDeadline` was not indexed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quorum_vote_deadline: Option<i64>,
    pub created_block: i64,
    pub created_tx: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_at_block: Option<i64>,
    /// Unix seconds after which the queued proposal may execute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executed_at_block: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canceled_at_block: Option<i64>,
    pub tallies: TalliesOut,
    pub vote_count: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProposalsPageOut {
    /// Newest first.
    pub proposals: Vec<ProposalSummaryOut>,
    /// Opaque. Present iff another page may follow; pass it back as `cursor`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// One call a passed proposal makes.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProposalActionOut {
    pub target: String,
    /// Wei, decimal.
    pub value: String,
    /// Empty for proposals made through `propose`, which carries no signature.
    pub signature: String,
    pub calldata: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProposalDetailOut {
    #[serde(flatten)]
    pub summary: ProposalSummaryOut,
    /// Verbatim. Attacker-controlled: render as text, never as HTML.
    pub description: String,
    pub actions: Vec<ProposalActionOut>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VoteOut {
    pub voter: String,
    /// 0 Against, 1 For, 2 Abstain.
    pub support: i16,
    /// Decimal uint256.
    pub weight: String,
    pub reason: String,
    pub block_number: i64,
    pub tx_hash: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VotesPageOut {
    /// Newest first.
    pub votes: Vec<VoteOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
