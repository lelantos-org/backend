//! The active subscriber set, parsed once and reused until the table changes.

use crate::repositories::subscriptions::{ActiveFingerprint, SubscriptionRow};
use ark_ed_on_bn254::Fr;
use std::collections::BTreeSet;
use std::sync::Arc;
use tracing::warn;

/// What a detection key that does not parse means, said once.
pub(super) const UNUSABLE_KEY: &str = "detection key is not gamma * 32 bytes; matches nothing";

/// One subscriber as `scan` consumes it: id, parsed detection key, gamma.
pub(super) type SubEntry = (i64, Arc<[Fr]>, usize);

/// Parse one subscriber's detection key, or `None` when it is not
/// `gamma * 32` bytes and therefore matches nothing.
pub(super) fn sub_entry(row: &SubscriptionRow) -> Option<SubEntry> {
    let gamma = row.gamma as usize;
    let dk = crypto::filter::parse_detection_key(&row.detection_key, gamma)?;
    Some((row.id, Arc::<[Fr]>::from(dk), gamma))
}

/// The parsed active subscriber set, with the fingerprint it was built from.
pub(super) struct SubscriberSet {
    fingerprint: ActiveFingerprint,
    pub(super) entries: Arc<[SubEntry]>,
    /// Subscriptions whose key did not parse. Carried so the warning still names
    /// them without re-parsing on every tick.
    pub(super) invalid: BTreeSet<i64>,
}

impl SubscriberSet {
    /// Whether this set still describes the table `fingerprint` was taken from.
    pub(super) fn matches(&self, fingerprint: ActiveFingerprint) -> bool {
        self.fingerprint == fingerprint
    }

    /// Reports ids only, never the key.
    pub(super) fn warn_unusable(&self) {
        if !self.invalid.is_empty() {
            warn!(subscription_ids = ?self.invalid, "{UNUSABLE_KEY}");
        }
    }

    /// Parse every row once, splitting off the keys that do not describe a
    /// `gamma * 32` byte detection key.
    pub(super) fn build(fingerprint: ActiveFingerprint, rows: &[SubscriptionRow]) -> Self {
        let mut entries = Vec::with_capacity(rows.len());
        let mut invalid = BTreeSet::new();
        for row in rows {
            match sub_entry(row) {
                Some(e) => entries.push(e),
                None => {
                    invalid.insert(row.id);
                }
            }
        }
        Self {
            fingerprint,
            entries: entries.into(),
            invalid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A subscription row with a well-formed `gamma * 32` byte key.
    fn sub(id: i64, gamma: i32) -> SubscriptionRow {
        SubscriptionRow {
            id,
            detection_key: vec![1u8; gamma as usize * 32],
            gamma,
            created_at: chrono::Utc::now(),
            active: true,
            backfilled_through_note_id: 0,
        }
    }

    #[test]
    fn a_key_of_the_wrong_width_does_not_parse() {
        let mut row = sub(1, 3);
        row.detection_key = vec![0u8; 2 * 32];
        assert!(sub_entry(&row).is_none(), "gamma is 3, key covers 2");
        assert!(sub_entry(&sub(1, 3)).is_some());
    }

    /// An unusable key is recorded once, at build time, and does not displace the
    /// subscribers that do parse.
    #[test]
    fn building_a_set_separates_unusable_keys() {
        let mut bad = sub(9, 3);
        bad.detection_key = vec![1u8; 31];

        let set = SubscriberSet::build((2, 9), &[sub(1, 3), bad]);

        assert_eq!(set.entries.len(), 1);
        assert_eq!(set.entries[0].0, 1);
        assert!(set.invalid.contains(&9));
    }

    #[test]
    fn an_unchanged_fingerprint_reuses_the_parsed_set() {
        let set = SubscriberSet::build((1, 1), &[sub(1, 3)]);
        assert!(set.matches((1, 1)));
    }

    /// A registration raises `max(id)`. Missing it would leave the new subscriber
    /// unscanned for as long as the process lives.
    #[test]
    fn a_new_subscription_invalidates_the_set() {
        let set = SubscriberSet::build((1, 1), &[sub(1, 3)]);
        assert!(!set.matches((2, 2)));
    }

    /// A deregistration lowers the count while `max(id)` can stay put, so the
    /// count is load-bearing. A stale entry here would make the subscription's
    /// `matches` insert fail the foreign key rather than conflict.
    #[test]
    fn a_deleted_subscription_invalidates_the_set() {
        let set = SubscriberSet::build((2, 2), &[sub(1, 3), sub(2, 3)]);
        assert!(!set.matches((1, 2)), "same max id, one fewer row");
    }
}
