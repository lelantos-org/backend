//! Database layer.
//!
//! Owns the diesel schema + migrations, the bb8 async pool, and shared
//! repository traits used by every indexer/webserver. Contains no business
//! logic and no domain enums beyond what the schema requires.
//!
//! Layering:
//!   - May import: `shared`.
//!   - Must NOT import: any indexer, webserver, relayer, or service crate.
//!   - Repository traits defined here are the canonical contract; per-crate
//!     impls should live next to their domain only when the storage shape
//!     differs from a shared concept.

pub mod advisory;
pub mod cursor;
pub mod listen;
pub mod migrate;
pub mod models;
pub mod pool;
pub mod reorg;
pub mod schema;

pub use advisory::{AdvisoryError, AdvisoryResult, ChainLock, chain_key};
pub use cursor::{CursorError, CursorRepo, CursorResult, PostgresCursorRepo, UpsertCursor};
pub use listen::{
    CHANNEL_NOTES_APPENDED, CHANNEL_RAW_EVENTS_APPENDED, CHANNEL_RAW_EVENTS_REORG, Wake,
};
pub use pool::{DbConn, DbPool, PoolCfg, PoolError, build_pool};
