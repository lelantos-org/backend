//! Which event a log's topic0 names.

use crate::abi::{
    AssetFeeSet, AssetMoved, AssetRegistered, DepositCanceled, DepositEscrowed, DepositFlushed,
    EmergencyUnwound, HaltedSet, NormalizedFeeSwept, NotePayload, NullifierConsumed,
    PerfFeeAccrued, ProposalCanceled, ProposalCreated, ProposalExecuted, ProposalQueued,
    ProposalQuorumVoteDeadline, Rebalanced, RootAdvanced, VoteCast, VoteCastWithParams,
    YieldAssetAdded, YieldParamsSet,
};
use alloy::primitives::B256;
use alloy::sol_types::SolEvent;
use shared::entities::EventKind;

/// Every event this crate decodes, by topic0.
///
/// One table for both lookups below. The ingester filters logs by
/// [`known_signatures`] and labels them with [`event_kind_from_topic0`]; two
/// separate lists could let an event be fetched and then not recognised.
const SIGNATURES: [(B256, EventKind); 23] = [
    (NotePayload::SIGNATURE_HASH, EventKind::NoteCreated),
    (AssetRegistered::SIGNATURE_HASH, EventKind::AssetRegistered),
    (AssetFeeSet::SIGNATURE_HASH, EventKind::AssetFeeSet),
    (RootAdvanced::SIGNATURE_HASH, EventKind::RootAdvanced),
    (AssetMoved::SIGNATURE_HASH, EventKind::AssetMoved),
    (
        NullifierConsumed::SIGNATURE_HASH,
        EventKind::NullifierConsumed,
    ),
    (DepositEscrowed::SIGNATURE_HASH, EventKind::DepositEscrowed),
    (DepositFlushed::SIGNATURE_HASH, EventKind::DepositFlushed),
    (DepositCanceled::SIGNATURE_HASH, EventKind::DepositCanceled),
    (YieldAssetAdded::SIGNATURE_HASH, EventKind::YieldAssetAdded),
    (YieldParamsSet::SIGNATURE_HASH, EventKind::YieldParamsSet),
    (PerfFeeAccrued::SIGNATURE_HASH, EventKind::PerfFeeAccrued),
    (
        NormalizedFeeSwept::SIGNATURE_HASH,
        EventKind::NormalizedFeeSwept,
    ),
    (Rebalanced::SIGNATURE_HASH, EventKind::Rebalanced),
    (HaltedSet::SIGNATURE_HASH, EventKind::HaltedSet),
    (
        EmergencyUnwound::SIGNATURE_HASH,
        EventKind::EmergencyUnwound,
    ),
    (ProposalCreated::SIGNATURE_HASH, EventKind::ProposalCreated),
    (
        ProposalQuorumVoteDeadline::SIGNATURE_HASH,
        EventKind::ProposalQuorumVoteDeadline,
    ),
    (VoteCast::SIGNATURE_HASH, EventKind::VoteCast),
    (
        VoteCastWithParams::SIGNATURE_HASH,
        EventKind::VoteCastWithParams,
    ),
    (ProposalQueued::SIGNATURE_HASH, EventKind::ProposalQueued),
    (
        ProposalExecuted::SIGNATURE_HASH,
        EventKind::ProposalExecuted,
    ),
    (
        ProposalCanceled::SIGNATURE_HASH,
        EventKind::ProposalCanceled,
    ),
];

pub fn event_kind_from_topic0(topic0: &B256) -> Option<EventKind> {
    SIGNATURES
        .iter()
        .find(|(sig, _)| sig == topic0)
        .map(|(_, kind)| *kind)
}

pub fn known_signatures() -> [B256; 23] {
    SIGNATURES.map(|(sig, _)| sig)
}
