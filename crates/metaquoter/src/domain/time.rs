use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock seconds since unix epoch; 0 if the clock reads before 1970.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
