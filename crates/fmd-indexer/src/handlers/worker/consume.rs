use crate::services::consume::ConsumeServiceImpl;
use database::listen::Wake;
use shared::shutdown::Shutdown;
use shared::tick;
use std::sync::Arc;

pub async fn run(
    svc: Arc<ConsumeServiceImpl>,
    tick_ms: u64,
    batch: i64,
    shutdown: Shutdown,
    wake: Option<Wake>,
) {
    tick::run_with_wake(svc, tick_ms, batch, shutdown, wake).await
}
