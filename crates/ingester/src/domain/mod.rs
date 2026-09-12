//! Pure types plus the decode and transform helpers over them.
//!
//! The bottom of the crate: no IO, no database, nothing from the layers above.

pub mod error;
pub mod models;

pub use error::{IngesterError, RpcError};
pub use models::{
    BlockCursor, Checkpoint, RawEvent, TickOutcome, parse_address, scanned_watermark,
};
