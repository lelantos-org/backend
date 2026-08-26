//! Cached view of the `assets` table.
//!
//! `explorer-indexer` writes it and this relayer reads it from two places:
//! `/chains`, which every wallet polls, and the shielded-fee check, which runs on
//! every submission. The pool is `PoolCfg::relayer()` with four connections, so a
//! per-request read from either is costly.
//!
//! The registry is append-mostly: an asset appears when the indexer first sees it
//! registered on chain and does not change afterwards. A short TTL therefore
//! picks up new assets promptly rather than correcting stale ones.

use crate::domain::error::{AppError, AppResult};
use crate::repositories::assets::{self, AssetRow};
use database::DbPool;
use moka::future::Cache;
use std::sync::Arc;
use std::time::Duration;

/// One entry per chain; a deployment serves a handful.
const CAPACITY: u64 = 64;

/// How long a chain's asset list is reused: short enough that a newly registered
/// asset becomes spendable within the minute, long enough that a herd of wallet
/// polls collapses onto one query.
const TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct AssetRegistry {
    pool: DbPool,
    by_chain: Cache<i64, Arc<Vec<AssetRow>>>,
}

impl AssetRegistry {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool,
            by_chain: shared::cache::build(CAPACITY, TTL),
        }
    }

    /// Every registered asset on `chain_id`, lowest id first.
    ///
    /// An empty list means the indexer has not caught up rather than that the
    /// chain supports no assets, matching what `/chains` documents for its
    /// `tokens` array.
    pub async fn for_chain(&self, chain_id: i64) -> AppResult<Arc<Vec<AssetRow>>> {
        let pool = self.pool.clone();
        self.by_chain
            .try_get_with(chain_id, async move {
                assets::list_for_chain(&pool, chain_id).await.map(Arc::new)
            })
            .await
            .map_err(|e: Arc<AppError>| AppError::Db(e.to_string()))
    }

    /// One asset by its MASP id, or `None` if this chain has no such asset.
    pub async fn by_asset_id(&self, chain_id: i64, asset_id: u64) -> AppResult<Option<AssetRow>> {
        let rows = self.for_chain(chain_id).await?;
        Ok(rows.iter().find(|a| a.asset_id() == asset_id).cloned())
    }
}
