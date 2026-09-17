//! Which `raw_events` kinds the consume loop fetches.

use shared::entities::EventKind;
use std::sync::OnceLock;

/// Whether this service consumes a kind.
///
/// The predicate, not a list — `kinds()` below is derived from it, so the
/// `WHERE event_kind = ANY` of the fetch cannot fall behind the decision. No
/// wildcard arm, so a new `EventKind` variant fails to compile here and has to
/// be classified deliberately.
///
/// explorer-indexer had the mirror-image bug: its filter was a hand-written
/// array, the yield kinds were added to the enum but not to it, and their
/// handlers were silently unreachable for as long as the mixin had been live.
/// This service reads the FMD zone plus the two kinds it needs for ordering.
const fn consumed(kind: EventKind) -> bool {
    match kind {
        EventKind::NoteCreated
        | EventKind::RootAdvanced
        | EventKind::NullifierConsumed
        | EventKind::DepositFlushed => true,
        EventKind::AssetRegistered
        | EventKind::AssetMoved
        | EventKind::DepositEscrowed
        | EventKind::DepositCanceled
        | EventKind::AssetFeeSet
        | EventKind::YieldAssetAdded
        | EventKind::YieldParamsSet
        | EventKind::PerfFeeAccrued
        | EventKind::NormalizedFeeSwept
        | EventKind::Rebalanced
        | EventKind::HaltedSet
        | EventKind::EmergencyUnwound
        | EventKind::ProposalCreated
        | EventKind::ProposalQuorumVoteDeadline
        | EventKind::VoteCast
        | EventKind::VoteCastWithParams
        | EventKind::ProposalQueued
        | EventKind::ProposalExecuted
        | EventKind::ProposalCanceled => false,
    }
}

/// The kinds this service fetches, as the `ANY` array wants them.
pub(super) fn kinds() -> &'static [i16] {
    static KINDS: OnceLock<Vec<i16>> = OnceLock::new();
    KINDS.get_or_init(|| {
        EventKind::ALL
            .into_iter()
            .filter(|k| consumed(*k))
            .map(EventKind::as_i16)
            .collect()
    })
}
