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
use tracing::warn;

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

/// The `pop_pending` projection. Narrower than the table: the flush path
/// needs the escrow digest preimage and the leaf, nothing else.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = deposit_escrowed_events)]
struct DepositRow {
    deposit_id: BigDecimal,
    cm: Vec<u8>,
    public_asset_id: i64,
    public_in: BigDecimal,
    fee_bps_at_submit: i32,
    payer: Vec<u8>,
    submitted_at_block: i64,
    cv_dep_x: BigDecimal,
    cv_dep_y: BigDecimal,
    rcv: BigDecimal,
}

impl TryFrom<DepositRow> for PendingDeposit {
    type Error = AppError;

    fn try_from(r: DepositRow) -> AppResult<Self> {
        let id = bigdecimal_to_u64(&r.deposit_id)?;
        Ok(PendingDeposit {
            id,
            cm: fixed_bytes(&r.cm, "cm")?,
            public_asset_id: r.public_asset_id as u64,
            public_in: bigdecimal_to_u64(&r.public_in)?,
            fee_bps_at_submit: u16::try_from(r.fee_bps_at_submit).map_err(|_| {
                AppError::Internal(format!(
                    "deposit {id}: fee_bps_at_submit {} out of u16 range",
                    r.fee_bps_at_submit
                ))
            })?,
            payer: fixed_bytes(&r.payer, "payer")?,
            // The contract hashed `uint32(block.number)`; anything wider
            // never matches the stored digest.
            submitted_at: u32::try_from(r.submitted_at_block).map_err(|_| {
                AppError::Internal(format!(
                    "deposit {id}: submitted_at_block {} out of u32 range",
                    r.submitted_at_block
                ))
            })?,
            cv_dep: [
                bigdecimal_to_u256(&r.cv_dep_x)?,
                bigdecimal_to_u256(&r.cv_dep_y)?,
            ],
            rcv: bigdecimal_to_u256(&r.rcv)?,
        })
    }
}

pub struct DepositMempool {
    pool: DbPool,
    chain_id: i64,
}

impl DepositMempool {
    pub fn new(pool: DbPool, chain_id: i64) -> Self {
        Self { pool, chain_id }
    }

    /// Return up to `limit` oldest pending deposits on this chain, skipping
    /// `exclude`.
    ///
    /// Exclusion happens in SQL rather than after the fact: quarantined
    /// deposits are the oldest ones by construction, so post-filtering would
    /// let them consume the whole `LIMIT` window and starve the batch.
    pub async fn pop_pending(
        &self,
        limit: usize,
        exclude: &[u64],
    ) -> AppResult<Vec<PendingDeposit>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;

        let mut query = deposit_escrowed_events::table
            .filter(deposit_escrowed_events::chain_id.eq(self.chain_id))
            .filter(deposit_escrowed_events::flushed_at_block.is_null())
            .filter(deposit_escrowed_events::canceled_at_block.is_null())
            .into_boxed();
        if !exclude.is_empty() {
            query = query.filter(deposit_escrowed_events::deposit_id.ne_all(to_id_bds(exclude)?));
        }
        let rows: Vec<DepositRow> = query
            // `deposit_id` breaks ties inside a block. Without it the subset
            // a limited flush picks — and the leaf order it commits — is
            // whatever the planner happens to return.
            .order((
                deposit_escrowed_events::submitted_at_block.asc(),
                deposit_escrowed_events::deposit_id.asc(),
            ))
            .limit(limit as i64)
            .select(DepositRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id_bd = row.deposit_id.clone();
            match PendingDeposit::try_from(row) {
                Ok(d) => out.push(d),
                // One unreadable row must not fail the query. It used to, and
                // since the flush worker re-runs the same query every tick
                // that meant a single malformed row stopped the chain from
                // ever flushing again.
                Err(e) => warn!(
                    chain_id = self.chain_id,
                    deposit_id = %id_bd,
                    error = %e,
                    "skipping unreadable pending deposit row"
                ),
            }
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

/// A `bytea` column the schema does not constrain to a width the code does.
/// `field` names the column so a bad row is diagnosable from the log alone.
fn fixed_bytes<const N: usize>(v: &[u8], field: &str) -> AppResult<[u8; N]> {
    v.try_into()
        .map_err(|_| AppError::Internal(format!("expected {N}-byte {field}, got {}", v.len())))
}
