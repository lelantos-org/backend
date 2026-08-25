// DB-backed query layer for pending escrowed deposits. Stateless: each call
// re-reads the canonical event ledger written by `explorer-indexer`.
//
// Pending = NOT (flushed OR canceled). Order by `submitted_at_block` so
// older deposits drain first.

use crate::adapters::calldata::LEAVES_PER_DEPOSIT;
use crate::adapters::numeric::{bigdecimal_to_u64, bigdecimal_to_u256};
use crate::domain::error::{AppError, AppResult};
use alloy::primitives::U256;
use bigdecimal::BigDecimal;
use bigdecimal::FromPrimitive;
use database::DbPool;
use database::schema::deposit_escrowed_events;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::Value as JsonValue;
use tracing::warn;

/// One escrowed deposit awaiting a flush.
///
/// A deposit occupies two leaves — the depositor's note and the note paying
/// whoever flushes it — so this carries a `cm` / `cv_dep` / `rcv` for each.
/// The fields are flat because they are a projection of the event row;
/// [`PendingDeposit::leaves`] is what pairs them up in tree order, and the
/// flush pipeline goes through it rather than reading the fields directly.
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
    /// The relayer's fee note — the second leaf the deposit mints, and the
    /// only thing that pays for the `flushBatch` gas.
    ///
    /// `fee_in`, `fee_cm` and `fee_cv_dep` are digest preimage, so they must
    /// read back exactly as escrowed. `fee_rcv` is not: it is the private
    /// blinder, needed to build that leaf's batch witness. `fee_aux` is the
    /// encrypted payload the relayer trial-decrypts to learn what it is being
    /// paid.
    pub fee_in: u64,
    pub fee_cm: [u8; 32],
    pub fee_cv_dep: [U256; 2],
    pub fee_rcv: U256,
    pub fee_aux: JsonValue,
}

/// One of the two leaves a deposit mints.
///
/// Both are denominated in the deposit's own asset — `_drainDeposit` requires
/// it — so `asset_id` is carried per leaf rather than looked up again by every
/// consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscrowLeaf {
    pub cm: [u8; 32],
    pub cv_dep: [U256; 2],
    pub asset_id: u64,
    pub public_in: u64,
    /// The leaf's `rcv_dep`: private witness for the batch circuit's per-leaf
    /// deposit binding, never part of the escrow digest.
    pub rcv: U256,
}

impl PendingDeposit {
    /// This deposit's leaves in the order `flushBatch` inserts them: the
    /// depositor's note, then the note paying whoever flushed it.
    ///
    /// Every leaf-indexed array the flush pipeline builds goes through here,
    /// so the order is decided once. `_drainDeposit` reads the pair back at
    /// exactly `2i` and `2i + 1` and rejects the batch if either is not a
    /// deposit leaf, so a transposition costs the whole batch its proof —
    /// which is why this is one named function rather than a tuple assembled
    /// at each call site.
    pub fn leaves(&self) -> [EscrowLeaf; LEAVES_PER_DEPOSIT] {
        [
            EscrowLeaf {
                cm: self.cm,
                cv_dep: self.cv_dep,
                asset_id: self.public_asset_id,
                public_in: self.public_in,
                rcv: self.rcv,
            },
            EscrowLeaf {
                cm: self.fee_cm,
                cv_dep: self.fee_cv_dep,
                asset_id: self.public_asset_id,
                public_in: self.fee_in,
                rcv: self.fee_rcv,
            },
        ]
    }
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
    fee_in: BigDecimal,
    fee_cm: Vec<u8>,
    fee_cv_dep_x: BigDecimal,
    fee_cv_dep_y: BigDecimal,
    fee_rcv: BigDecimal,
    fee_aux: JsonValue,
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
            // The contract narrows `feeIn` to `uint48` before hashing it, so
            // a wider value could never have been escrowed in the first place.
            fee_in: bigdecimal_to_u64(&r.fee_in)?,
            fee_cm: fixed_bytes(&r.fee_cm, "fee_cm")?,
            fee_cv_dep: [
                bigdecimal_to_u256(&r.fee_cv_dep_x)?,
                bigdecimal_to_u256(&r.fee_cv_dep_y)?,
            ],
            fee_rcv: bigdecimal_to_u256(&r.fee_rcv)?,
            fee_aux: r.fee_aux,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn deposit() -> PendingDeposit {
        PendingDeposit {
            id: 1,
            cm: [0xaa; 32],
            public_asset_id: 7,
            public_in: 1_000,
            fee_bps_at_submit: 25,
            payer: [0xcd; 20],
            submitted_at: 99,
            cv_dep: [U256::from(1), U256::from(2)],
            rcv: U256::from(3),
            fee_in: 250,
            fee_cm: [0xbb; 32],
            fee_cv_dep: [U256::from(4), U256::from(5)],
            fee_rcv: U256::from(6),
            fee_aux: JsonValue::Null,
        }
    }

    /// The order is what `_drainDeposit` reads back at `2i` and `2i + 1`.
    /// Swapping the pair builds a batch that proves and then reverts, so this
    /// pins it rather than leaving it to each call site.
    #[test]
    fn test_leaves_puts_the_depositors_note_before_the_fee_note() {
        let d = deposit();
        let [principal, fee] = d.leaves();

        assert_eq!(principal.cm, d.cm);
        assert_eq!(principal.public_in, d.public_in);
        assert_eq!(principal.rcv, d.rcv);

        assert_eq!(fee.cm, d.fee_cm);
        assert_eq!(fee.public_in, d.fee_in);
        assert_eq!(fee.rcv, d.fee_rcv);
    }

    /// `_drainDeposit` requires both leaves to name the deposit's asset, and
    /// the fee note has no asset field of its own to disagree with.
    #[test]
    fn test_both_leaves_carry_the_deposits_asset() {
        let d = deposit();
        assert!(d.leaves().iter().all(|l| l.asset_id == d.public_asset_id));
    }
}
