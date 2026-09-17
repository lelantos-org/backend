//! Orchestration. One module per route's answer, plus the venue-APY measurement.
//!
//! The read modules ([`assets`], [`chains`], [`governance`], [`prices`],
//! [`yield_index`]) build
//! the body a route serves and are free of axum; [`venue_apy`] is the write
//! path's measurement, driven by [`crate::handlers::worker`].

pub mod assets;
pub mod chains;
pub mod governance;
pub mod prices;
pub mod venue_apy;
pub mod yield_index;

/// Read a key through a cache, running the loader exactly once on a miss.
///
/// Every cached route here has that shape, and so does every other webserver in
/// the workspace, so the helper lives in `shared::cache` rather than being
/// restated per crate.
pub(crate) use shared::cache::cached;
