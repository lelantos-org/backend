//! Mirror readings published for `/chains`.
//!
//! Its own module so the atomics can be called as plain methods: the parent
//! glob-imports `diesel::prelude`, whose `RunQueryDsl::load` shadows the
//! inherent `Atomic*::load`.

use fmd_crypto::tree::Field;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Mirror readings published for `/chains` to read without the mutex.
///
/// Each field is updated independently, which suffices for a display endpoint: a
/// reader catching a mutation mid-flight sees a root one advance away from the
/// count, no worse than the staleness of not holding the lock.
#[derive(Debug, Default)]
pub struct MirrorSnapshot {
    leaf_count: AtomicU64,
    root: parking_lot::RwLock<Field>,
    desynced: AtomicBool,
}

impl MirrorSnapshot {
    pub fn leaf_count(&self) -> u64 {
        self.leaf_count.load(Ordering::Relaxed)
    }

    pub fn root(&self) -> Field {
        *self.root.read()
    }

    pub fn is_desynced(&self) -> bool {
        self.desynced.load(Ordering::Relaxed)
    }

    /// Publish a fresh set of readings. Only [`super::TreeMirror`] calls this, and
    /// only while holding the mirror.
    pub(super) fn publish(&self, leaf_count: u64, root: Option<Field>, desynced: bool) {
        self.leaf_count.store(leaf_count, Ordering::Relaxed);
        self.desynced.store(desynced, Ordering::Relaxed);
        // A root that cannot be computed leaves the last good one in place rather
        // than publishing zero; the mirror is heading for a park in that case.
        if let Some(root) = root {
            *self.root.write() = root;
        }
    }
}
