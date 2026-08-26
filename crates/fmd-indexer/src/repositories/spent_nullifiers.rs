use crate::domain::error::{Result, log_unique_violation};
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
/// `/v1/chains/{id}/nullifiers/chunks/*` slices on `seq`, mirroring
/// `notes.leaf_index`. The chain provides no index for nullifiers, so `seq` is
/// derived from (block_number, log_index) order at insert time.
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

/// One row of the dedup-and-high-water probe. `max_seq` repeats on every row;
/// the coordinates are `NULL` on the single row a range with no matches yields.
#[derive(QueryableByName)]
struct SeqProbeRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    max_seq: i64,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    block_number: Option<i64>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    log_index: Option<i32>,
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
    /// Rows already stored are dropped before numbering: a crash between insert
    /// and cursor upsert replays the batch, and letting a duplicate consume an
    /// ordinal would collide the rest of the batch on
    /// `spent_nullifiers_chain_seq_idx`.
    ///
    /// This is a read-then-write with no transaction, so it is correct only while
    /// one process consumes a given chain. That is enforced by the per-chain
    /// advisory lock in `adapters::locks::ChainLocks`, checked at the top of every
    /// consume tick, not by this function. Without it, a peer committing between
    /// the two reads leaves `stored` stale while `max(seq)` is fresh, and
    /// surviving rows are numbered past the gap (`seq = 0, 1, 4`) without error.
    async fn insert_batch(&self, rows: &[NewSpentNullifier]) -> Result<usize> {
        let Some(first) = rows.first() else {
            return Ok(0);
        };
        let chain_id = first.chain_id;
        // Bound the dedup read to the batch's own block range. `>= min_block`
        // alone is open-ended, so a replay from the start of the chain would load
        // most of the table on every batch: O(n^2) over a full rewind, for a
        // check that concerns only these blocks.
        let (min_block, max_block) = rows.iter().fold((i64::MAX, i64::MIN), |(lo, hi), r| {
            (lo.min(r.block_number), hi.max(r.block_number))
        });

        let mut conn = super::conn(&self.pool).await?;

        // The already-stored coordinates and the chain's high-water `seq` in one
        // statement. The `LEFT JOIN` onto a one-row aggregate keeps `max_seq`
        // available even when the range holds nothing, and reading both at the
        // same instant makes them a consistent pair rather than two snapshots a
        // round trip apart.
        let observed = diesel::sql_query(
            "WITH hi AS ( \
                 SELECT COALESCE(MAX(seq), -1) AS max_seq \
                   FROM spent_nullifiers WHERE chain_id = $1 \
             ) \
             SELECT hi.max_seq, s.block_number, s.log_index \
               FROM hi \
               LEFT JOIN spent_nullifiers s \
                 ON s.chain_id = $1 \
                AND s.block_number >= $2 \
                AND s.block_number <= $3",
        )
        .bind::<diesel::sql_types::BigInt, _>(chain_id)
        .bind::<diesel::sql_types::BigInt, _>(min_block)
        .bind::<diesel::sql_types::BigInt, _>(max_block)
        .load::<SeqProbeRow>(&mut conn)
        .await?;

        // Through the slice: `diesel::prelude` puts a `first` in scope that
        // would otherwise shadow the one wanted here.
        let next_seq = observed.as_slice().first().map_or(0, |r| r.max_seq + 1);
        let stored: HashSet<(i64, i32)> = observed
            .iter()
            .filter_map(|r| Some((r.block_number?, r.log_index?)))
            .collect();

        let mut fresh: Vec<&NewSpentNullifier> = rows
            .iter()
            .filter(|r| !stored.contains(&(r.block_number, r.log_index)))
            .collect();
        if fresh.is_empty() {
            return Ok(0);
        }
        fresh.sort_by_key(|r| (r.block_number, r.log_index));

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
            .await
            .inspect_err(|e| log_unique_violation("spent_nullifiers", e))?;

        // Emitted here rather than from the consume tick, since `seq` is assigned
        // in this function and not visible to the caller. `values` is the numbered
        // set, so its last element carries the new high-water mark.
        if let Some(last) = values.last() {
            metrics::gauge!(
                shared::metrics::name::SPENT_NULLIFIERS_SEQ_MAX,
                "chain_id" => chain_id.to_string(),
            )
            .set(last.seq as f64);
        }
        Ok(n)
    }

    async fn delete_from_block(&self, chain_id: i64, from_block: i64) -> Result<usize> {
        let mut conn = super::conn(&self.pool).await?;
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
