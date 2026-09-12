//! Pure crate types and column decoders. No IO, no DB.

pub mod address;
pub mod error;

pub use error::{ProtocolIndexerError, Result};
