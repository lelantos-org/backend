//! Database I/O, one module per table this crate reads or writes.
//!
//! The consume and filter cursors go through `database`'s shared `CursorRepo`,
//! per `ARCHITECTURE.md`.

use crate::domain::error::{FmdIndexerError, Result};
use database::{DbConn, DbPool};

pub mod matches;
pub mod notes;
pub mod raw_events;
pub mod spent_nullifiers;
pub mod subscriptions;
pub mod tree_state;

/// Check a connection out of the pool.
///
/// Every repository method starts with this, so the pool-error mapping is written
/// once and each method opens with the query it runs.
pub(crate) async fn conn(pool: &DbPool) -> Result<DbConn<'_>> {
    pool.get()
        .await
        .map_err(|e| FmdIndexerError::Db(e.to_string()))
}

/// Surface a unique violation that the statement's `ON CONFLICT` target did
/// not cover.
///
/// `notes` and `spent_nullifiers` each carry a second UNIQUE beyond the one the
/// insert names (`notes_chain_leaf_idx`, `spent_nullifiers_chain_seq_idx` and
/// `spent_nullifiers_chain_id_nf_key`). A collision there is not absorbed by
/// `DO NOTHING`: it aborts the statement, the tick fails, and the driver logs a
/// generic tick error while the cursor stops advancing. Naming the constraint
/// makes the cause visible.
pub(crate) fn log_unique_violation(table: &str, e: &diesel::result::Error) {
    use diesel::result::{DatabaseErrorKind, Error as DieselError};
    if let DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, info) = e {
        tracing::error!(
            table,
            constraint = info.constraint_name().unwrap_or("<unknown>"),
            detail = info.details().unwrap_or(""),
            "insert hit a unique constraint the ON CONFLICT target does not cover"
        );
    }
}
