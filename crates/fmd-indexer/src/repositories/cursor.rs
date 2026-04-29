//! Re-export of the shared cursor repository.
//!
//! Canonical definition lives in [`database::cursor`]. Kept as a thin
//! re-export so existing imports inside this crate stay stable.

pub use database::{CursorRepo, PostgresCursorRepo, UpsertCursor};
