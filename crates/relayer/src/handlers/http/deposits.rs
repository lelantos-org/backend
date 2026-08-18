// SSE stream of deposit lifecycle events. Webapp subscribes per chain id;
// `FlushPipeline` publishes via `EventBroadcaster` after each successful
// flushBatch. Heartbeats keep the connection alive across reverse proxies.

use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use futures::StreamExt;
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub chain_id: i64,
}

pub async fn deposits_stream(
    State(st): State<AppState>,
    Query(q): Query<StreamQuery>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    // An unconfigured chain would otherwise get a perfectly valid stream that
    // can never emit anything, which reads to a client as "no deposits yet".
    if !st.serves_chain(q.chain_id) {
        return Err(AppError::UnknownChain(q.chain_id));
    }
    let rx = st.events.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(move |res| {
        let chain_id = q.chain_id;
        async move {
            let ev = match res {
                Ok(ev) => ev,
                // Lagged: receiver fell behind broadcast capacity. Skip
                // missed events; client falls back to its 5-min timeout.
                Err(_) => return None,
            };
            if ev.chain_id() != chain_id {
                return None;
            }
            let payload = serde_json::to_string(&ev).ok()?;
            Some(Ok(Event::default().data(payload)))
        }
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    ))
}
