//! This crate uses the shared HTTP error type.
//!
//! Unlike `relayer` and `metaquoter`, nothing here needs a variant the shared
//! one does not carry.

pub use shared::http::{AppError, AppResult};
