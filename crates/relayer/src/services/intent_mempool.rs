// DB-backed query layer for pending escrow intents. Stateless: each call
// re-reads the canonical event ledger written by `explorer-indexer`.
//
// Pending = NOT (flushed OR canceled). Order by `submitted_at_block` so
// older intents drain first.

use crate::domain::error::{AppError, AppResult};
use alloy::primitives::U256;
use bigdecimal::BigDecimal;
use bigdecimal::FromPrimitive;
use bigdecimal::ToPrimitive;
use database::DbPool;
use database::schema::intent_escrowed_events;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone)]
pub struct PendingIntent {
    pub id: u64,
    pub cm0: [u8; 32],
    pub cm1: [u8; 32],
    pub public_asset_id: u64,
    pub public_in: u64,
    /// `feeBpsAtSubmit` from the `IntentEscrowed` event. Required to
    /// rebuild the on-chain digest in `flushBatch` — the contract no
    /// longer stores it.
    pub fee_bps_at_submit: u16,
    pub cv_dep0: [U256; 2],
    pub cv_dep1: [U256; 2],
    pub rcv_total: U256,
}

pub struct IntentMempool {
    pool: DbPool,
    chain_id: i64,
}

impl IntentMempool {
    pub fn new(pool: DbPool, chain_id: i64) -> Self {
        Self { pool, chain_id }
    }

    /// Return up to `limit` oldest pending intents on this chain.
    pub async fn pop_pending(&self, limit: usize) -> AppResult<Vec<PendingIntent>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;

        type Row = (
            BigDecimal,
            Vec<u8>,
            Vec<u8>,
            i64,
            BigDecimal,
            i32,
            BigDecimal,
            BigDecimal,
            BigDecimal,
            BigDecimal,
            BigDecimal,
        );
        let rows: Vec<Row> = intent_escrowed_events::table
            .filter(intent_escrowed_events::chain_id.eq(self.chain_id))
            .filter(intent_escrowed_events::flushed_at_block.is_null())
            .filter(intent_escrowed_events::canceled_at_block.is_null())
            .order(intent_escrowed_events::submitted_at_block.asc())
            .limit(limit as i64)
            .select((
                intent_escrowed_events::intent_id,
                intent_escrowed_events::cm0,
                intent_escrowed_events::cm1,
                intent_escrowed_events::public_asset_id,
                intent_escrowed_events::public_in,
                intent_escrowed_events::fee_bps_at_submit,
                intent_escrowed_events::cv_dep0_x,
                intent_escrowed_events::cv_dep0_y,
                intent_escrowed_events::cv_dep1_x,
                intent_escrowed_events::cv_dep1_y,
                intent_escrowed_events::rcv_total,
            ))
            .load(&mut conn)
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for (id_bd, cm0, cm1, asset, public_in_bd, fbps, cv0x, cv0y, cv1x, cv1y, rcv) in rows {
            let id = id_bd.to_u64().ok_or_else(|| {
                AppError::Internal(format!("intent_id {} out of u64 range", id_bd))
            })?;
            let public_in = public_in_bd.to_u64().ok_or_else(|| {
                AppError::Internal(format!("public_in {} out of u64 range", public_in_bd))
            })?;
            let fee_bps_at_submit = u16::try_from(fbps).map_err(|_| {
                AppError::Internal(format!("fee_bps_at_submit {} out of u16 range", fbps))
            })?;
            out.push(PendingIntent {
                id,
                cm0: vec_to_arr32(&cm0)?,
                cm1: vec_to_arr32(&cm1)?,
                public_asset_id: asset as u64,
                public_in,
                fee_bps_at_submit,
                cv_dep0: [bd_to_u256(&cv0x)?, bd_to_u256(&cv0y)?],
                cv_dep1: [bd_to_u256(&cv1x)?, bd_to_u256(&cv1y)?],
                rcv_total: bd_to_u256(&rcv)?,
            });
        }
        Ok(out)
    }

    /// Mark `ids` as flushed at `block_number`. Idempotent: ingester later
    /// overwrites with the canonical `IntentFlushed` block_number, but the
    /// optimistic write is enough to keep them out of `pop_pending` while
    /// the indexer catches up.
    pub async fn mark_submitted(&self, ids: &[u64], block_number: i64) -> AppResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;
        let id_bds: Vec<BigDecimal> = ids
            .iter()
            .map(|id| {
                BigDecimal::from_u64(*id)
                    .ok_or_else(|| AppError::Internal(format!("intent_id {} unrepresentable", id)))
            })
            .collect::<AppResult<_>>()?;
        let n = diesel::update(
            intent_escrowed_events::table
                .filter(intent_escrowed_events::chain_id.eq(self.chain_id))
                .filter(intent_escrowed_events::intent_id.eq_any(id_bds))
                .filter(intent_escrowed_events::flushed_at_block.is_null()),
        )
        .set(intent_escrowed_events::flushed_at_block.eq(Some(block_number)))
        .execute(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))?;
        Ok(n)
    }
}

fn bd_to_u256(bd: &BigDecimal) -> AppResult<U256> {
    // Numeric columns are always non-negative integers in this schema (BN254
    // coords and BJJ scalars). BigDecimal → decimal string → U256.
    let s = bd.to_string();
    U256::from_str_radix(&s, 10)
        .map_err(|e| AppError::Internal(format!("u256 parse of {}: {}", s, e)))
}

fn vec_to_arr32(v: &[u8]) -> AppResult<[u8; 32]> {
    if v.len() != 32 {
        return Err(AppError::Internal(format!(
            "expected 32-byte cm, got {}",
            v.len()
        )));
    }
    let mut a = [0u8; 32];
    a.copy_from_slice(v);
    Ok(a)
}
