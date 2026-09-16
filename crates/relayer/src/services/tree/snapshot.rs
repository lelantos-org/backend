//! Mirror readings published for `/chains`.
//!
//! Its own module because it is the only part of the mirror read without the
//! mutex, and the only part `/chains` touches.

use crypto::tree::Field;
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
    pub(super) fn publish(&self, leaf_count: u64, root: Field, desynced: bool) {
        self.leaf_count.store(leaf_count, Ordering::Relaxed);
        self.desynced.store(desynced, Ordering::Relaxed);
        *self.root.write() = root;
    }
}
