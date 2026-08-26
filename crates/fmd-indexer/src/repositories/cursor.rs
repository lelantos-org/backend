//! Re-export of the shared cursor repository.
//!
//! The canonical definition lives in [`database::cursor`]; this is a thin
//! re-export so imports inside this crate stay stable.

pub use database::{CursorRepo, PostgresCursorRepo, UpsertCursor};
