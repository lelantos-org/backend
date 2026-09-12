//! Orchestration: each module owns one endpoint's read path over the
//! repositories and the caches. See `backend/ARCHITECTURE.md`.

pub mod chunks;
pub mod commitments;
pub mod head;
pub mod matches;
pub mod notes;
pub mod nullifiers;
pub mod subscriptions;
pub mod tree;

/// The wire-format helpers these services map rows with. They are IO-free
/// transforms and live in `domain`; re-exported here so `services::field` and
/// `services::poseidon` keep resolving.
pub use crate::domain::{field, poseidon};

/// Read a key from a cache, running the loader once on a miss, and report the
/// outcome under a `metric` label.
///
/// Every cached read here has that shape, and the unmetered form is shared with
/// the other webservers, so both live in `shared::cache` rather than being
/// restated per crate. Aliased to the name this crate's call sites already use.
pub(crate) use shared::cache::cached_metered as cached;
