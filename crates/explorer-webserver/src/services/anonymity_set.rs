use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::AnonymitySetOut;
use crate::repositories::anonymity_set;
use std::sync::Arc;

/// Start of the recency window, floored to the hour containing `now_ts`.
///
/// Quantised because it is part of the cache key. Taken from the wall clock
/// directly it would change every second, so every request would miss and the
/// analytic TTL would never do anything. Losing up to an hour of precision on a
/// window measured in days costs nothing.
fn recent_from(now_ts: i64, recent_sec: i64) -> i64 {
    now_ts.div_euclid(3600) * 3600 - recent_sec
}

pub async fn denominations(
    st: &AppState,
    chain_id: Option<i64>,
    asset_id_u64: Option<i64>,
    limit: i64,
    now_ts: i64,
    recent_sec: i64,
) -> AppResult<Arc<Vec<AnonymitySetOut>>> {
    let recent_from_ts = recent_from(now_ts, recent_sec);
    let key = (chain_id, asset_id_u64, limit, recent_from_ts);
    let pool = st.pool.clone();
    st.cache
        .anonymity_set
        .try_get_with(key, async move {
            let rows =
                anonymity_set::denominations(&pool, chain_id, asset_id_u64, limit, recent_from_ts)
                    .await?;
            let out: Vec<AnonymitySetOut> = rows
                .into_iter()
                .map(|r| AnonymitySetOut {
                    chain_id: r.chain_id,
                    asset_id_u64: r.asset_id_u64,
                    public_out: r.public_out,
                    count: r.count,
                    recent_count: r.recent_count,
                    first_ts: r.first_ts,
                    last_ts: r.last_ts,
                })
                .collect();
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOON: i64 = 1_786_812_896;

    #[test]
    fn the_window_start_is_the_lookback_before_the_current_hour() {
        let hour = NOON.div_euclid(3600) * 3600;
        assert_eq!(recent_from(NOON, 86_400), hour - 86_400);
    }

    /// The whole reason it is quantised: `recent_from_ts` is part of the cache
    /// key, so taking it from the wall clock unrounded would miss on every
    /// request and the analytic TTL would never apply.
    #[test]
    fn every_second_of_an_hour_yields_one_cache_key() {
        let hour = NOON.div_euclid(3600) * 3600;
        for offset in [0, 1, 1_800, 3_599] {
            assert_eq!(recent_from(hour + offset, 86_400), hour - 86_400);
        }
        assert_ne!(recent_from(hour + 3_600, 86_400), hour - 86_400);
    }
}
