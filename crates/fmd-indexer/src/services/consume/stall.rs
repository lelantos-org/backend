//! How long each chain has been parked on the same cursor.

use std::collections::HashMap;
use tokio::sync::Mutex;
use tracing::error;

/// Consecutive no-progress ticks before deferral is treated as a stall rather
/// than a normal wait for the next block. One minute at the default tick.
const STALL_TICKS: u32 = 120;
/// Re-report cadence once stalled, so a wedged chain stays visible without
/// filling the log at tick rate.
const STALL_REPEAT_TICKS: u32 = STALL_TICKS * 10;

/// How long each chain has been parked on the same cursor.
///
/// Deferring a transaction is normal for one tick and an outage after a thousand.
/// The tick returns `Ok(())` either way, so this counter distinguishes them.
#[derive(Default)]
pub(super) struct StallTracker(Mutex<HashMap<i64, Stall>>);

struct Stall {
    cursor: i64,
    ticks: u32,
}

impl StallTracker {
    pub(super) async fn record_idle(&self, chain_id: i64, cursor: i64, rows: usize) {
        let mut stalls = self.0.lock().await;
        let stall = stalls.entry(chain_id).or_insert(Stall { cursor, ticks: 0 });
        if stall.cursor != cursor {
            *stall = Stall { cursor, ticks: 0 };
        }
        stall.ticks += 1;

        let overdue = stall.ticks.checked_sub(STALL_TICKS);
        if overdue.is_some_and(|n| n % STALL_REPEAT_TICKS == 0) {
            error!(
                chain_id,
                cursor,
                rows,
                ticks = stall.ticks,
                "consume has committed nothing for {} consecutive ticks; the head tx cannot be \
                 completed (missing DepositEscrowed, or a tx wider than the batch window)",
                stall.ticks
            );
        }
    }

    pub(super) async fn clear(&self, chain_id: i64) {
        self.0.lock().await.remove(&chain_id);
    }
}
