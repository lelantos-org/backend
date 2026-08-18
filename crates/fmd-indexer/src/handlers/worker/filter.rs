use crate::services::filter::FilterServiceImpl;
use shared::shutdown::Shutdown;
use shared::tick;
use std::sync::Arc;

pub async fn run(svc: Arc<FilterServiceImpl>, tick_ms: u64, batch: i64, shutdown: Shutdown) {
    tick::run(svc, tick_ms, batch, shutdown).await
}
