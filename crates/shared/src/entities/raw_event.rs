use crate::chain::ChainId;
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
    ];

    /// Length of [`ALL`](Self::ALL), pinned to the highest discriminant.
    ///
    /// The discriminants are `1..=N` with no gaps, so the last variant's value
    /// *is* the count. Adding a variant without extending `ALL` then fails to
    /// compile on the array length rather than silently shortening the set.
    pub const ALL_COUNT: usize = Self::EmergencyUnwound as usize;

    pub fn from_i16(v: i16) -> Option<Self> {
        // Derived from `ALL` rather than a second 16-arm match: the two lists
        // drifting apart is exactly the failure this enum has already had once.
        Self::ALL.into_iter().find(|k| k.as_i16() == v)
    }

    pub fn as_i16(self) -> i16 {
        self as i16
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEvent {
    pub id: i64,
    pub chain_id: ChainId,
    pub block_number: i64,
    pub block_hash: Vec<u8>,
    pub block_ts: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub event_kind: EventKind,
    pub topics: Vec<Vec<u8>>,
    pub data: Vec<u8>,
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

    #[test]
    fn from_i16_round_trips_every_variant() {
        for kind in EventKind::ALL {
            assert_eq!(EventKind::from_i16(kind.as_i16()), Some(kind));
        }
        assert_eq!(EventKind::from_i16(0), None);
        assert_eq!(EventKind::from_i16(EventKind::ALL_COUNT as i16 + 1), None);
    }
}
