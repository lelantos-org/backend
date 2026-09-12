use crate::domain::error::ProtocolIndexerError;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::assets;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// One `AssetRegistered`, as [`upsert_batch`] applies it.
///
/// Omits `decimals` and `symbol`, so replaying an `AssetRegistered` never
/// clears a value the metadata sweep already fetched.
#[derive(Debug, Clone, Insertable, AsChangeset)]
#[diesel(table_name = assets)]
pub struct UpsertAsset {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub token: Vec<u8>,
    pub scale: BigDecimal,
}

/// Register a tick's assets over one pooled connection, in event order.
///
/// One statement per row rather than one multi-row `ON CONFLICT DO UPDATE`,
/// which Postgres rejects when a single statement would touch the same key
/// twice. Registrations are rare — once per asset, ever — so the checkout, not
/// the round trip, was the cost worth removing here.
pub async fn upsert_batch(pool: &DbPool, rows: &[UpsertAsset]) -> Result<(), ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = super::conn(pool).await?;
    for row in rows {
        diesel::insert_into(assets::table)
            .values(row)
            .on_conflict((assets::chain_id, assets::asset_id_u64))
            .do_update()
            .set(row)
            .execute(&mut conn)
            .await?;
    }
    Ok(())
}

/// Apply a tick's rate changes over one pooled connection, in event order.
pub async fn upsert_fee_batch(
    pool: &DbPool,
    rows: &[UpsertAssetFee],
) -> Result<(), ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = super::conn(pool).await?;
    for row in rows {
        diesel::insert_into(assets::table)
            .values((
                row,
                assets::token.eq(Vec::<u8>::new()),
                assets::scale.eq(BigDecimal::from(0)),
            ))
            .on_conflict((assets::chain_id, assets::asset_id_u64))
            .do_update()
            .set(row)
            .execute(&mut conn)
            .await?;
    }
    Ok(())
}

/// Per-leg rates for one asset, keyed like `UpsertAsset`.
///
/// A separate insert rather than fields on `UpsertAsset`, because the two
/// events are independent: `AssetFeeSet` fires again on every rate change,
/// long after registration, and must not restate `token` or `scale`. The
/// insert branch exists only for ordering — the contract emits both events in
/// one transaction, but nothing here depends on which row lands first.
#[derive(Debug, Clone, Insertable, AsChangeset)]
#[diesel(table_name = assets)]
pub struct UpsertAssetFee {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub deposit_bps: i16,
    pub withdraw_bps: i16,
}

/// One asset still missing at least one metadata column, with what it already
/// has so the sweep only reads what it needs.
#[derive(Debug, Clone, Queryable)]
pub struct PendingMetadata {
    pub asset_id_u64: i64,
    pub token: Vec<u8>,
    pub decimals: Option<i16>,
    pub symbol: Option<String>,
}

/// Values to write back. `None` means not fetched this round and is skipped by
/// `AsChangeset`, so a failed `symbol()` never clears a stored `decimals`, and
/// the reverse.
#[derive(Debug, Default, AsChangeset)]
#[diesel(table_name = assets)]
pub struct AssetMetadata {
    pub decimals: Option<i16>,
    pub symbol: Option<String>,
}

impl AssetMetadata {
    /// Nothing was resolved, so no update is issued.
    pub fn is_empty(&self) -> bool {
        self.decimals.is_none() && self.symbol.is_none()
    }
}

/// Assets on `chain_id` whose `decimals` or `symbol` has not been fetched yet.
pub async fn missing_metadata(
    pool: &DbPool,
    chain_id: i64,
    limit: i64,
) -> Result<Vec<PendingMetadata>, ProtocolIndexerError> {
    let mut conn = super::conn(pool).await?;
    assets::table
        .filter(assets::chain_id.eq(chain_id))
        .filter(assets::decimals.is_null().or(assets::symbol.is_null()))
        .order(assets::asset_id_u64.asc())
        .limit(limit)
        .select((
            assets::asset_id_u64,
            assets::token,
            assets::decimals,
            assets::symbol,
        ))
        .load(&mut conn)
        .await
        .map_err(Into::into)
}

pub async fn set_metadata(
    pool: &DbPool,
    chain_id: i64,
    asset_id_u64: i64,
    meta: AssetMetadata,
) -> Result<(), ProtocolIndexerError> {
    let mut conn = super::conn(pool).await?;
    diesel::update(
        assets::table
            .filter(assets::chain_id.eq(chain_id))
            .filter(assets::asset_id_u64.eq(asset_id_u64)),
    )
    .set(meta)
    .execute(&mut conn)
    .await?;
    Ok(())
}
