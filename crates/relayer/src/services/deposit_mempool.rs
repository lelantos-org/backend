// DB-backed query layer for pending escrowed deposits. Stateless: each call
// re-reads the canonical event ledger written by `explorer-indexer`.
//
// Pending = NOT (flushed OR canceled). Order by `submitted_at_block` so
// older deposits drain first.

use crate::adapters::numeric::{bigdecimal_to_u64, bigdecimal_to_u256};
use crate::domain::error::{AppError, AppResult};
use alloy::primitives::U256;
use bigdecimal::BigDecimal;
use bigdecimal::FromPrimitive;
use database::DbPool;
use database::schema::deposit_escrowed_events;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// One escrowed deposit awaiting a flush. A deposit occupies exactly one
/// leaf, so this carries a single `cm` / `cv_dep` / `rcv`.
#[derive(Debug, Clone)]
pub struct PendingDeposit {
    pub id: u64,
    pub cm: [u8; 32],
    pub public_asset_id: u64,
    pub public_in: u64,
    /// `feeBpsAtSubmit` from the `DepositEscrowed` event. Part of the
    /// on-chain digest preimage, which `flushBatch` re-derives from the
    /// `DepositMeta` the relayer replays — the contract stores only the
    /// digest.
    pub fee_bps_at_submit: u16,
    /// Digest preimage fields the contract no longer keeps. `submitted_at`
    /// is the `DepositEscrowed` block number, narrowed to the `uint32` the
    /// contract hashed.
    pub payer: [u8; 20],
    pub submitted_at: u32,
    pub cv_dep: [U256; 2],
    pub rcv: U256,
}

pub struct DepositMempool {
    pool: DbPool,
    chain_id: i64,
}

impl DepositMempool {
    pub fn new(pool: DbPool, chain_id: i64) -> Self {
        Self { pool, chain_id }
    }

    /// Return up to `limit` oldest pending deposits on this chain.
    pub async fn pop_pending(&self, limit: usize) -> AppResult<Vec<PendingDeposit>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;

        type Row = (
            BigDecimal,
            Vec<u8>,
            i64,
            BigDecimal,
            i32,
            Vec<u8>,
            i64,
            BigDecimal,
            BigDecimal,
            BigDecimal,
        );
        let rows: Vec<Row> = deposit_escrowed_events::table
            .filter(deposit_escrowed_events::chain_id.eq(self.chain_id))
            .filter(deposit_escrowed_events::flushed_at_block.is_null())
            .filter(deposit_escrowed_events::canceled_at_block.is_null())
            // `deposit_id` breaks ties inside a block. Without it the subset
            // a limited flush picks — and the leaf order it commits — is
            // whatever the planner happens to return.
            .order((
                deposit_escrowed_events::submitted_at_block.asc(),
                deposit_escrowed_events::deposit_id.asc(),
            ))
            .limit(limit as i64)
            .select((
                deposit_escrowed_events::deposit_id,
                deposit_escrowed_events::cm,
                deposit_escrowed_events::public_asset_id,
                deposit_escrowed_events::public_in,
                deposit_escrowed_events::fee_bps_at_submit,
                deposit_escrowed_events::payer,
                deposit_escrowed_events::submitted_at_block,
                deposit_escrowed_events::cv_dep_x,
                deposit_escrowed_events::cv_dep_y,
                deposit_escrowed_events::rcv,
            ))
            .load(&mut conn)
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for (id_bd, cm, asset, public_in_bd, fbps, payer, submitted_at, cvx, cvy, rcv) in rows {
            let id = bigdecimal_to_u64(&id_bd)?;
            let public_in = bigdecimal_to_u64(&public_in_bd)?;
            let fee_bps_at_submit = u16::try_from(fbps).map_err(|_| {
                AppError::Internal(format!("fee_bps_at_submit {} out of u16 range", fbps))
            })?;
            // The contract hashed `uint32(block.number)`; anything wider
            // never matches the stored digest.
            let submitted_at = u32::try_from(submitted_at).map_err(|_| {
                AppError::Internal(format!(
                    "submitted_at_block {} out of u32 range for deposit {}",
                    submitted_at, id
                ))
            })?;
            out.push(PendingDeposit {
                id,
                cm: vec_to_arr32(&cm)?,
                public_asset_id: asset as u64,
                public_in,
                fee_bps_at_submit,
                payer: vec_to_arr20(&payer)?,
                submitted_at,
                cv_dep: [bigdecimal_to_u256(&cvx)?, bigdecimal_to_u256(&cvy)?],
                rcv: bigdecimal_to_u256(&rcv)?,
            });
        }
        Ok(out)
    }

    /// Mark `ids` as flushed at `block_number`. Idempotent: ingester later
    /// overwrites with the canonical `DepositFlushed` block_number, but the
    /// optimistic write is enough to keep them out of `pop_pending` while
    /// the indexer catches up.
    ///
    /// Returns the number of rows actually claimed. A short count means some
    /// ids stopped being unflushed between `pop_pending` and here. Usually the
    /// indexer got there first — it writes the canonical `DepositFlushed` row
    /// unconditionally, and `submit` only returns after one confirmation, so
    /// the event is often already ingested. The other cause is a second relayer
    /// on this chain, which `pop_pending`'s plain SELECT does not guard
    /// against; telling the two apart needs `count_unflushed`.
    pub async fn mark_submitted(&self, ids: &[u64], block_number: i64) -> AppResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;
        let id_bds = to_id_bds(ids)?;
        let n = diesel::update(
            deposit_escrowed_events::table
                .filter(deposit_escrowed_events::chain_id.eq(self.chain_id))
                .filter(deposit_escrowed_events::deposit_id.eq_any(id_bds))
                .filter(deposit_escrowed_events::flushed_at_block.is_null()),
        )
        .set(deposit_escrowed_events::flushed_at_block.eq(Some(block_number)))
        .execute(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))?;
        Ok(n)
    }

    /// How many of `ids` still carry no `flushed_at_block`. Zero after a short
    /// `mark_submitted` means the indexer wrote the canonical flush first;
    /// anything above zero means those rows were never claimed by anyone.
    pub async fn count_unflushed(&self, ids: &[u64]) -> AppResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;
        let id_bds = to_id_bds(ids)?;
        let n: i64 = deposit_escrowed_events::table
            .filter(deposit_escrowed_events::chain_id.eq(self.chain_id))
            .filter(deposit_escrowed_events::deposit_id.eq_any(id_bds))
            .filter(deposit_escrowed_events::flushed_at_block.is_null())
            .count()
            .get_result(&mut conn)
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;
        Ok(n as usize)
    }
}

fn to_id_bds(ids: &[u64]) -> AppResult<Vec<BigDecimal>> {
    ids.iter()
        .map(|id| {
            BigDecimal::from_u64(*id)
                .ok_or_else(|| AppError::Internal(format!("deposit_id {} unrepresentable", id)))
        })
        .collect()
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

fn vec_to_arr20(v: &[u8]) -> AppResult<[u8; 20]> {
    if v.len() != 20 {
        return Err(AppError::Internal(format!(
            "expected 20-byte address, got {}",
            v.len()
        )));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(v);
    Ok(a)
}
