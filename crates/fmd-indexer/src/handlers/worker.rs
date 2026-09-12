//! The tick-loop entry point.
//!
//! One function for both services: a worker here is a `TickService` wired to
//! `shared::tick::run_with_wake` and nothing more, so consume and filter differ
//! only in the value passed in.

use database::listen::Wake;
use shared::shutdown::Shutdown;
use shared::tick::{self, TickService};
use std::sync::Arc;

pub async fn run<S: TickService + 'static>(
    svc: Arc<S>,
    tick_ms: u64,
    batch: i64,
    shutdown: Shutdown,
    wake: Option<Wake>,
) {
    tick::run_with_wake(svc, tick_ms, batch, shutdown, wake).await
}
