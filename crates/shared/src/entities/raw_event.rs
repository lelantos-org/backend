use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
pub enum EventKind {
    NoteCreated = 1,
    AssetRegistered = 2,
    RootAdvanced = 3,
    AssetMoved = 4,
    NullifierConsumed = 5,
    DepositEscrowed = 6,
    DepositFlushed = 7,
    DepositCanceled = 8,
    AssetFeeSet = 9,
    YieldAssetAdded = 10,
    YieldParamsSet = 11,
    PerfFeeAccrued = 12,
    NormalizedFeeSwept = 13,
    Rebalanced = 14,
    HaltedSet = 15,
    EmergencyUnwound = 16,
    ProposalCreated = 17,
    ProposalQuorumVoteDeadline = 18,
    VoteCast = 19,
    VoteCastWithParams = 20,
    ProposalQueued = 21,
    ProposalExecuted = 22,
    ProposalCanceled = 23,
}

impl EventKind {
    /// Every variant, ascending.
    ///
    /// The one place the set is written down. A consumer that reads a subset —
    /// explorer-indexer and fmd-indexer each do — derives its filter from this
    /// by partitioning it, rather than restating the members in a second
    /// hand-maintained list. That is not a stylistic preference: the yield kinds
    /// were added to the enum and to explorer-indexer's `apply` match but not to
    /// its `WHERE event_kind = ANY` list, so the arms were unreachable,
    /// `asset_yield` stayed permanently empty, and the cursor wedged whenever the
    /// newest event was a yield one.
    ///
    /// `ALL_COUNT` below is what makes a forgotten entry here fail the build.
    pub const ALL: [Self; Self::ALL_COUNT] = [
        Self::NoteCreated,
        Self::AssetRegistered,
        Self::RootAdvanced,
        Self::AssetMoved,
        Self::NullifierConsumed,
        Self::DepositEscrowed,
        Self::DepositFlushed,
        Self::DepositCanceled,
        Self::AssetFeeSet,
        Self::YieldAssetAdded,
        Self::YieldParamsSet,
        Self::PerfFeeAccrued,
        Self::NormalizedFeeSwept,
        Self::Rebalanced,
        Self::HaltedSet,
        Self::EmergencyUnwound,
        Self::ProposalCreated,
        Self::ProposalQuorumVoteDeadline,
        Self::VoteCast,
        Self::VoteCastWithParams,
        Self::ProposalQueued,
        Self::ProposalExecuted,
        Self::ProposalCanceled,
    ];

    /// Length of [`ALL`](Self::ALL), pinned to the highest discriminant.
    ///
    /// The discriminants are `1..=N` with no gaps, so the last variant's value
    /// *is* the count. Adding a variant without extending `ALL` then fails to
    /// compile on the array length rather than silently shortening the set.
    pub const ALL_COUNT: usize = Self::ProposalCanceled as usize;

    pub fn from_i16(v: i16) -> Option<Self> {
        // Derived from `ALL` rather than a second 16-arm match: the two lists
        // drifting apart is exactly the failure this enum has already had once.
        Self::ALL.into_iter().find(|k| k.as_i16() == v)
    }

    pub fn as_i16(self) -> i16 {
        self as i16
    }

    /// Which consumer owns the derived state this event produces.
    ///
    /// The partition [`ALL`](Self::ALL) refers to, written once. Each indexer
    /// derives its `WHERE event_kind = ANY(...)` filter from
    /// [`kinds_for`](Self::kinds_for) rather than restating a list, so a new
    /// variant cannot be handled by one consumer and silently fetched by none —
    /// the failure that once left `asset_yield` permanently empty.
    ///
    /// Exhaustive with no wildcard arm: adding a variant fails the build here
    /// until it is consciously assigned.
    pub const fn consumer(self) -> Consumer {
        match self {
            // Note and nullifier state.
            Self::NoteCreated | Self::NullifierConsumed => Consumer::Fmd,

            // The asset catalog and yield bindings the wallet boots from, plus
            // the two ledgers the relayer drains: `tree_advances` bootstraps its
            // Merkle mirror and `deposit_escrowed_events` feeds its flush
            // pipeline. Those two look like explorer analytics and are not.
            Self::AssetRegistered
            | Self::AssetFeeSet
            | Self::YieldAssetAdded
            | Self::YieldParamsSet
            | Self::HaltedSet
            | Self::RootAdvanced
            | Self::DepositEscrowed
            | Self::DepositFlushed
            | Self::DepositCanceled => Consumer::Protocol,

            // The governor's proposal and vote ledger. Emitted by the governor
            // rather than the pool, so protocol-indexer also checks the emitter.
            Self::ProposalCreated
            | Self::ProposalQuorumVoteDeadline
            | Self::VoteCast
            | Self::VoteCastWithParams
            | Self::ProposalQueued
            | Self::ProposalExecuted
            | Self::ProposalCanceled => Consumer::Protocol,

            // Flow analytics and the fee ledger behind them.
            Self::AssetMoved | Self::PerfFeeAccrued | Self::NormalizedFeeSwept => {
                Consumer::Explorer
            }

            // Decoded, but writing no derived state anywhere. Fetched by no
            // consumer: a cursor advances past them on the next event it does
            // fetch.
            Self::Rebalanced | Self::EmergencyUnwound => Consumer::None,
        }
    }

    /// The kinds `consumer` reads, ascending.
    pub fn kinds_for(consumer: Consumer) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|k| k.consumer() == consumer)
            .collect()
    }
}

/// Which indexer owns the state an [`EventKind`] produces.
///
/// One event, one owner. A table is written by exactly one consumer, so a kind
/// that fed two would mean two writers racing on one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Consumer {
    /// State the wallet boots from and the relayer transacts against.
    Protocol,
    /// Analytics served to explorer-ui.
    Explorer,
    /// Notes and spent nullifiers.
    Fmd,
    /// Produces no derived state.
    None,
}

#[cfg(test)]
mod event_kind_tests {
    use super::EventKind;

    /// `ALL` really is every variant, and in discriminant order.
    ///
    /// The array length is already pinned to the highest discriminant, so this
    /// catches the remaining way to get it wrong: listing a variant twice and
    /// omitting another, which keeps the length right.
    #[test]
    fn all_is_complete_and_ordered() {
        for (i, kind) in EventKind::ALL.into_iter().enumerate() {
            assert_eq!(
                kind.as_i16(),
                i as i16 + 1,
                "ALL[{i}] is {kind:?}; discriminants must be 1..=N with no gaps or repeats"
            );
        }
    }

    /// The four sets must partition `ALL`: every kind assigned, none twice. A
    /// kind belonging to no consumer would be fetched by nobody and its arm
    /// unreachable; one belonging to two would mean two writers on one table.
    #[test]
    fn every_consumer_set_together_partitions_all() {
        use super::Consumer;

        let sets = [
            Consumer::Protocol,
            Consumer::Explorer,
            Consumer::Fmd,
            Consumer::None,
        ]
        .map(EventKind::kinds_for);

        let total: usize = sets.iter().map(Vec::len).sum();
        assert_eq!(
            total,
            EventKind::ALL_COUNT,
            "the sets must cover every kind exactly once"
        );

        let mut seen: Vec<i16> = sets.iter().flatten().map(|k| k.as_i16()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), EventKind::ALL_COUNT, "a kind appears twice");
    }

    /// The two ledgers that look like explorer analytics but are read by the
    /// relayer's write path: `tree_advances` bootstraps its Merkle mirror and
    /// `deposit_escrowed_events` feeds its flush pipeline. Classing them as
    /// analytics would put that path behind a service that may lag.
    #[test]
    fn the_relayers_ledgers_belong_to_the_protocol_consumer() {
        use super::Consumer;

        for kind in [
            EventKind::RootAdvanced,
            EventKind::DepositEscrowed,
            EventKind::DepositFlushed,
            EventKind::DepositCanceled,
        ] {
            assert_eq!(kind.consumer(), Consumer::Protocol, "{kind:?}");
        }
    }

    #[test]
    fn from_i16_round_trips_every_variant() {
        for kind in EventKind::ALL {
            assert_eq!(EventKind::from_i16(kind.as_i16()), Some(kind));
        }
        assert_eq!(EventKind::from_i16(0), None);
        assert_eq!(EventKind::from_i16(EventKind::ALL_COUNT as i16 + 1), None);
    }
}
