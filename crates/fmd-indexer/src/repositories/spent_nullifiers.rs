use crate::domain::error::{FmdIndexerError, Result};
use async_trait::async_trait;
use database::DbPool;
use database::schema::spent_nullifiers;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct NewSpentNullifier {
    pub chain_id: i64,
    pub block_number: i64,
    pub log_index: i32,
    pub nf: Vec<u8>,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}

/// `NewSpentNullifier` plus the dense per-chain ordinal the repo assigns.
/// `seq` is what `/v1/chains/{id}/nullifiers/chunks/*` slices on, mirroring
/// `notes.leaf_index` — but the chain gives us no index for nullifiers, so
/// it is derived from (block_number, log_index) order at insert time.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = spent_nullifiers)]
struct SeqSpentNullifier {
    chain_id: i64,
    block_number: i64,
    log_index: i32,
    nf: Vec<u8>,
    tx_hash: Vec<u8>,
    block_ts: i64,
    seq: i64,
}

#[async_trait]
pub trait SpentNullifiersRepo: Send + Sync {
    async fn insert_batch(&self, rows: &[NewSpentNullifier]) -> Result<usize>;
    async fn delete_from_block(&self, chain_id: i64, from_block: i64) -> Result<usize>;
}

pub struct PostgresSpentNullifiersRepo {
    pool: DbPool,
}

impl PostgresSpentNullifiersRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SpentNullifiersRepo for PostgresSpentNullifiersRepo {
    /// Assign each new row a dense `seq` continuing the chain's sequence.
    ///
    /// Rows already stored must be dropped *before* numbering: a crash
    /// between insert and cursor upsert replays the batch, and letting a
    /// duplicate consume an ordinal would collide the rest of the batch on
    /// `spent_nullifiers_chain_seq_idx`.
    ///
    /// This is a read-then-write with no transaction, so it is only correct
    /// while one process consumes a given chain. That is enforced by the
    /// per-chain advisory lock in `adapters::locks::ChainLocks`, checked at the
    /// top of every consume tick — not by anything in this function. Without
    /// it, a peer committing between the two reads leaves `stored` stale while
    /// `max(seq)` is fresh, and surviving rows get numbered past the gap
    /// (`seq = 0, 1, 4`) with no error raised.
    async fn insert_batch(&self, rows: &[NewSpentNullifier]) -> Result<usize> {
        let Some(first) = rows.first() else {
            return Ok(0);
        };
        let chain_id = first.chain_id;
        let min_block = rows
            .iter()
            .map(|r| r.block_number)
            .min()
            .expect("rows non-empty");

        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;

        let stored: HashSet<(i64, i32)> = spent_nullifiers::table
            .filter(spent_nullifiers::chain_id.eq(chain_id))
            .filter(spent_nullifiers::block_number.ge(min_block))
            .select((spent_nullifiers::block_number, spent_nullifiers::log_index))
            .load::<(i64, i32)>(&mut conn)
            .await?
            .into_iter()
            .collect();

        let mut fresh: Vec<&NewSpentNullifier> = rows
            .iter()
            .filter(|r| !stored.contains(&(r.block_number, r.log_index)))
            .collect();
        if fresh.is_empty() {
            return Ok(0);
        }
        fresh.sort_by_key(|r| (r.block_number, r.log_index));

        let next_seq = spent_nullifiers::table
            .filter(spent_nullifiers::chain_id.eq(chain_id))
            .select(diesel::dsl::max(spent_nullifiers::seq))
            .first::<Option<i64>>(&mut conn)
            .await?
            .map_or(0, |max| max + 1);

        let values: Vec<SeqSpentNullifier> = fresh
            .into_iter()
            .enumerate()
            .map(|(i, r)| SeqSpentNullifier {
                chain_id: r.chain_id,
                block_number: r.block_number,
                log_index: r.log_index,
                nf: r.nf.clone(),
                tx_hash: r.tx_hash.clone(),
                block_ts: r.block_ts,
                seq: next_seq + i as i64,
            })
            .collect();

        let n = diesel::insert_into(spent_nullifiers::table)
            .values(&values)
            .on_conflict((
                spent_nullifiers::chain_id,
                spent_nullifiers::block_number,
                spent_nullifiers::log_index,
            ))
            .do_nothing()
            .execute(&mut conn)
            .await?;
        Ok(n)
    }

    async fn delete_from_block(&self, chain_id: i64, from_block: i64) -> Result<usize> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        let n = diesel::delete(
            spent_nullifiers::table
                .filter(spent_nullifiers::chain_id.eq(chain_id))
                .filter(spent_nullifiers::block_number.ge(from_block)),
        )
        .execute(&mut conn)
        .await?;
        Ok(n)
    }
}
