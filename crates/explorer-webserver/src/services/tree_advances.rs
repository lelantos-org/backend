use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{ChainFlowOut, CountPoint, TreeAdvanceOut};
use crate::repositories::{chains, tree_advances};
use std::collections::BTreeMap;
use std::sync::Arc;

pub async fn list(
    st: &AppState,
    chain_id: Option<i64>,
    since_start_index: Option<i64>,
    limit: i64,
) -> AppResult<Arc<Vec<TreeAdvanceOut>>> {
    let key = (chain_id, since_start_index, limit);
    let pool = st.pool.clone();
    st.cache
        .tree_advances
        .try_get_with(key, async move {
            let rows = tree_advances::list(&pool, chain_id, since_start_index, limit).await?;
            let out: Vec<TreeAdvanceOut> = rows
                .into_iter()
                .map(|t| TreeAdvanceOut {
                    chain_id: t.chain_id,
                    block_number: t.block_number,
                    log_index: t.log_index,
                    start_index: t.start_index,
                    inserted: t.inserted,
                    old_root_hex: hex::encode(&t.old_root),
                    new_root_hex: hex::encode(&t.new_root),
                    tx_hash_hex: hex::encode(&t.tx_hash),
                    block_ts: t.block_ts,
                })
                .collect();
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

pub async fn tx_counts(
    st: &AppState,
    chain_id: Option<i64>,
    bucket_sec: i64,
    since_ts: Option<i64>,
) -> AppResult<Arc<Vec<CountPoint>>> {
    let key = (chain_id, bucket_sec, since_ts);
    let pool = st.pool.clone();
    st.cache
        .tx_counts
        .try_get_with(key, async move {
            let rows = tree_advances::count_buckets(&pool, chain_id, bucket_sec, since_ts).await?;
            let out: Vec<CountPoint> = rows
                .into_iter()
                .map(|r| CountPoint {
                    ts: r.ts,
                    count: r.count,
                })
                .collect();
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

const HOURS: usize = 24;

/// Oldest hour bucket of the window, and the slot-0 anchor the SQL projects
/// against. Anchored on the hour containing `now_ts` so that hour lands in slot
/// 23; anchoring on `now_ts - 86_400` would span 25 distinct hours and force the
/// newest into slot 23 alongside the hour before it.
fn window_start(now_ts: i64) -> i64 {
    let current_hour = now_ts.div_euclid(3600) * 3600;
    current_hour - (HOURS as i64 - 1) * 3600
}

fn empty_chain(chain_id: i64) -> ChainFlowOut {
    ChainFlowOut {
        chain_id,
        inflow: 0,
        outflow: 0,
        hourly_in: vec![0; HOURS],
        hourly_out: vec![0; HOURS],
        tx_count: 0,
    }
}

/// One entry per indexed chain, hottest first.
///
/// Every chain in `indexed` is emitted, at zero when it saw no insertions in the
/// window, so a client can distinguish a scanned but quiet chain from an
/// unindexed one. A chain carrying rows is emitted whether or not it appears in
/// `indexed`, so no data is dropped to match the list.
///
/// Rows outside the window are dropped rather than clamped: a clamped slot would
/// add a foreign hour's count to an edge bucket and read as real activity.
fn fold_chain_flows(
    rows: Vec<tree_advances::ChainFlow24hRow>,
    indexed: Vec<i64>,
) -> Vec<ChainFlowOut> {
    let mut map: BTreeMap<i64, ChainFlowOut> = indexed
        .into_iter()
        .map(|chain_id| (chain_id, empty_chain(chain_id)))
        .collect();
    for r in rows {
        let Ok(slot) = usize::try_from(r.slot) else {
            continue;
        };
        if slot >= HOURS {
            continue;
        }
        let entry = map
            .entry(r.chain_id)
            .or_insert_with(|| empty_chain(r.chain_id));
        entry.hourly_in[slot] += r.count;
        entry.tx_count += r.count;
    }
    let mut out: Vec<ChainFlowOut> = map.into_values().collect();
    // Chain id breaks ties so the quiet chains, all at zero, keep a stable order
    // between requests.
    out.sort_by_key(|b| (std::cmp::Reverse(b.tx_count), b.chain_id));
    out
}

pub async fn chain_flows_24h(st: &AppState, now_ts: i64) -> AppResult<Arc<Vec<ChainFlowOut>>> {
    let hour_start = window_start(now_ts);
    let pool = st.pool.clone();
    st.cache
        .chain_flows_24h
        .try_get_with(hour_start, async move {
            let (rows, indexed) = tokio::try_join!(
                tree_advances::chain_flows_24h(&pool, hour_start),
                chains::indexed(&pool),
            )?;
            Ok::<_, AppError>(Arc::new(fold_chain_flows(rows, indexed)))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::tree_advances::ChainFlow24hRow;

    fn row(chain_id: i64, slot: i32, count: i64) -> ChainFlow24hRow {
        ChainFlow24hRow {
            chain_id,
            slot,
            count,
        }
    }

    #[test]
    fn window_spans_exactly_24_hours_ending_at_the_current_one() {
        // 12:34:56 UTC on some day.
        let now: i64 = 1_786_812_896;
        let start = window_start(now);
        assert_eq!(start % 3600, 0);
        let current_hour = now.div_euclid(3600) * 3600;
        assert_eq!((current_hour - start) / 3600, HOURS as i64 - 1);
    }

    #[test]
    fn the_current_hour_lands_in_the_last_slot() {
        let now: i64 = 1_786_812_896;
        let current_hour = now.div_euclid(3600) * 3600;
        let slot = (current_hour - window_start(now)) / 3600;
        assert_eq!(slot, 23);
    }

    fn chain_ids(out: &[ChainFlowOut]) -> Vec<i64> {
        out.iter().map(|c| c.chain_id).collect()
    }

    #[test]
    fn folds_counts_into_their_own_slots() {
        let out = fold_chain_flows(vec![row(1, 0, 5), row(1, 23, 7)], vec![1]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].hourly_in[0], 5);
        assert_eq!(out[0].hourly_in[23], 7);
        assert_eq!(out[0].tx_count, 12);
    }

    #[test]
    fn drops_slots_outside_the_window() {
        // A block timestamped ahead of the node clock must not inflate the
        // newest bucket.
        let out = fold_chain_flows(vec![row(1, 23, 7), row(1, 24, 99), row(1, -1, 99)], vec![1]);
        assert_eq!(out[0].hourly_in[23], 7);
        assert_eq!(out[0].tx_count, 7);
    }

    #[test]
    fn orders_chains_by_descending_tx_count() {
        let out = fold_chain_flows(vec![row(1, 0, 1), row(2, 0, 9)], vec![1, 2]);
        assert_eq!(chain_ids(&out), vec![2, 1]);
    }

    #[test]
    fn an_indexed_chain_with_no_insertions_reports_zero_rather_than_vanishing() {
        // A quiet chain is a measurement; an absent one reads as unindexed.
        let out = fold_chain_flows(vec![row(1, 0, 5)], vec![1, 10, 42161]);
        assert_eq!(chain_ids(&out), vec![1, 10, 42161]);
        let quiet = &out[1];
        assert_eq!(quiet.tx_count, 0);
        assert_eq!(quiet.hourly_in, vec![0; HOURS]);
    }

    #[test]
    fn quiet_chains_keep_a_stable_order_between_requests() {
        let out = fold_chain_flows(vec![row(7, 0, 3)], vec![42161, 10, 7, 1]);
        assert_eq!(chain_ids(&out), vec![7, 1, 10, 42161]);
    }

    #[test]
    fn a_chain_with_rows_is_kept_even_when_it_is_not_in_the_indexed_list() {
        // Rows are never dropped to match the indexed list.
        let out = fold_chain_flows(vec![row(99, 0, 4)], vec![1]);
        assert_eq!(chain_ids(&out), vec![99, 1]);
    }

    #[test]
    fn no_indexed_chains_and_no_rows_is_an_empty_grid() {
        assert!(fold_chain_flows(vec![], vec![]).is_empty());
    }
}
