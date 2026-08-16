use crate::app::AppState;
use crate::domain::amount::whole_tokens;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{KindCounts, TxKind, TxOut};
use crate::repositories::transactions::{self, KindCountRow};
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::warn;

pub async fn recent(
    st: &AppState,
    chain_id: Option<i64>,
    since_ts: Option<i64>,
    limit: i64,
) -> AppResult<Arc<Vec<TxOut>>> {
    let key = (chain_id, since_ts, limit);
    let cache = st.cache.transactions.clone();
    let pool = st.pool.clone();
    cache
        .try_get_with(key, async move {
            let rows = transactions::recent(&pool, chain_id, since_ts, limit).await?;
            let out: Vec<TxOut> = rows
                .into_iter()
                .filter_map(|r| {
                    // An unrecognised kind means the SQL and this enum have
                    // drifted; drop the row rather than mislabel it.
                    let Some(kind) = TxKind::parse(&r.kind) else {
                        warn!(kind = %r.kind, "unknown transaction kind");
                        return None;
                    };
                    Some(TxOut {
                        chain_id: r.chain_id,
                        tx_hash_hex: hex::encode(&r.tx_hash),
                        block_number: r.block_number,
                        block_ts: r.block_ts,
                        kind,
                        asset_id_u64: r.asset_id_u64,
                        amount: r
                            .amount
                            .as_ref()
                            .and_then(|a| whole_tokens(a, r.decimals))
                            .map(|a| a.normalized().to_string()),
                    })
                })
                .collect();
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

pub async fn kind_counts(
    st: &AppState,
    chain_id: Option<i64>,
    bucket_sec: i64,
    since_ts: Option<i64>,
) -> AppResult<Arc<Vec<KindCounts>>> {
    let key = (chain_id, bucket_sec, since_ts);
    let cache = st.cache.tx_kinds.clone();
    let pool = st.pool.clone();
    cache
        .try_get_with(key, async move {
            let rows = transactions::kind_counts(&pool, chain_id, bucket_sec, since_ts).await?;
            Ok::<_, AppError>(Arc::new(fold(rows)))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

/// Pivot (bucket, kind, count) rows into one row per bucket.
///
/// A kind absent from a bucket is a real zero — the SQL only emits rows for
/// kinds that occurred — so the series stay aligned for a stacked chart.
fn fold(rows: Vec<KindCountRow>) -> Vec<KindCounts> {
    let mut buckets: BTreeMap<i64, KindCounts> = BTreeMap::new();
    for r in rows {
        let Some(kind) = TxKind::parse(&r.kind) else {
            warn!(kind = %r.kind, "unknown transaction kind");
            continue;
        };
        let b = buckets.entry(r.ts).or_insert(KindCounts {
            ts: r.ts,
            ..Default::default()
        });
        match kind {
            TxKind::Deposit => b.deposit += r.count,
            TxKind::Pending => b.pending += r.count,
            TxKind::Transfer => b.transfer += r.count,
            TxKind::Withdraw => b.withdraw += r.count,
        }
    }
    buckets.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ts: i64, kind: &str, count: i64) -> KindCountRow {
        KindCountRow {
            ts,
            kind: kind.to_string(),
            count,
        }
    }

    #[test]
    fn pivots_kinds_into_one_row_per_bucket() {
        let out = fold(vec![
            row(100, "deposit", 4),
            row(100, "transfer", 19),
            row(100, "withdraw", 3),
            row(100, "pending", 2),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(
            (
                out[0].deposit,
                out[0].pending,
                out[0].transfer,
                out[0].withdraw
            ),
            (4, 2, 19, 3)
        );
    }

    #[test]
    fn an_absent_kind_is_zero_not_missing() {
        let out = fold(vec![row(100, "transfer", 5)]);
        assert_eq!(out[0].deposit, 0);
        assert_eq!(out[0].withdraw, 0);
    }

    #[test]
    fn buckets_are_ascending() {
        let out = fold(vec![row(300, "deposit", 1), row(100, "deposit", 1)]);
        assert_eq!(out.iter().map(|b| b.ts).collect::<Vec<_>>(), vec![100, 300]);
    }

    #[test]
    fn an_unknown_kind_is_dropped_not_miscounted() {
        let out = fold(vec![row(100, "deposit", 1), row(100, "sideways", 99)]);
        assert_eq!(out[0].deposit, 1);
        assert_eq!(out[0].pending + out[0].transfer + out[0].withdraw, 0);
    }
}
