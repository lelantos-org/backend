//! The `tree_advances` read, for the accepted-root window.
//!
//! fmd-indexer writes one row per on-chain advance. The relayer reads the most
//! recent of them at boot so a wallet holding a proof against a root the chain
//! still accepts is not refused for naming one this process never held.

use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use database::schema::tree_advances;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// The `limit` newest published roots for `chain_id`, newest first, so the head
/// is the root the mirror must currently agree with.
pub async fn recent_roots(pool: &DbPool, chain_id: i64, limit: i64) -> AppResult<Vec<Vec<u8>>> {
    let mut conn = super::conn(pool).await?;
    tree_advances::table
        .filter(tree_advances::chain_id.eq(chain_id))
        .order(tree_advances::start_index.desc())
        .limit(limit)
        .select(tree_advances::new_root)
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
