//! Live-tail worker handler.
//!
//! Thin loop: tick + sleep. All orchestration lives in
//! [`crate::services::live::LiveService`].

use crate::domain::error::IngesterError;
use crate::services::live::LiveService;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

pub async fn run(svc: Arc<dyn LiveService>) -> Result<(), IngesterError> {
    info!(
        chain_id = svc.chain_id(),
        poll_ms = svc.poll_ms(),
        "live mode start"
    );
    loop {
        svc.tick().await?;
        tokio::time::sleep(Duration::from_millis(svc.poll_ms())).await;
    }
}
