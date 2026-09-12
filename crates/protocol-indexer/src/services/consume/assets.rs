//! The asset catalog: `AssetRegistered` and `AssetFeeSet` into `assets`.
//!
//! Pure up to [`AssetPlan::apply`] — the push methods build rows and touch no
//! database.

use crate::domain::error::ProtocolIndexerError;
use crate::repositories::assets::{self, UpsertAsset, UpsertAssetFee};
use alloy::primitives::{Address, U256};
use chain_types::numeric::u256_to_bigdecimal;
use database::DbPool;

/// One window's writes to `assets`.
#[derive(Debug, Default)]
pub struct AssetPlan {
    pub registered: Vec<UpsertAsset>,
    pub fees: Vec<UpsertAssetFee>,
}

impl AssetPlan {
    pub fn push_registered(&mut self, chain_id: i64, asset_id: u64, token: Address, scale: U256) {
        self.registered.push(UpsertAsset {
            chain_id,
            asset_id_u64: asset_id as i64,
            token: token.as_slice().to_vec(),
            scale: u256_to_bigdecimal(scale),
        });
    }

    /// Rates are mutable, so this replaces whatever was stored rather than
    /// filling a gap: a later `AssetFeeSet` for the same asset is a rate change,
    /// not a duplicate.
    pub fn push_fee(&mut self, chain_id: i64, asset_id: u64, deposit_bps: u16, withdraw_bps: u16) {
        self.fees.push(UpsertAssetFee {
            chain_id,
            asset_id_u64: asset_id as i64,
            // Both are `uint16` on chain but bounded by `MAX_FEE_BPS` (2000),
            // so the cast to Postgres `SMALLINT` cannot lose a valid value.
            deposit_bps: deposit_bps as i16,
            withdraw_bps: withdraw_bps as i16,
        });
    }

    pub async fn apply(&self, pool: &DbPool) -> Result<(), ProtocolIndexerError> {
        // Registration before rates: `upsert_fee_batch` inserts a placeholder row
        // when the asset is missing, and running it first would leave `token` and
        // `scale` blank until the registration overwrote them.
        assets::upsert_batch(pool, &self.registered).await?;
        assets::upsert_fee_batch(pool, &self.fees).await?;
        Ok(())
    }
}
