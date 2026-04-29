// Process-wide pub/sub for relayer-side intent lifecycle events.
//
// `FlushPipeline` publishes `IntentEvent::Flushed` after each successful
// `flushBatch` submission; the SSE handler in `handlers::http::intents`
// fans the events out to subscribed webapp clients.
//
// Backed by a `tokio::sync::broadcast` channel — bounded queue, lagging
// receivers drop oldest events. 256 slots is enough for steady-state
// flush rates (≤ MAX_N_BATCH per tick, default 8).

use serde::Serialize;
use tokio::sync::broadcast::{self, Receiver, Sender};

const CAPACITY: usize = 256;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum IntentEvent {
    Flushed {
        intent_id: u64,
        chain_id: i64,
        tx_hash: String,
        block_number: i64,
    },
}

impl IntentEvent {
    pub fn chain_id(&self) -> i64 {
        match self {
            IntentEvent::Flushed { chain_id, .. } => *chain_id,
        }
    }
}

#[derive(Clone)]
pub struct EventBroadcaster {
    tx: Sender<IntentEvent>,
}

impl EventBroadcaster {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        Self { tx }
    }

    /// Best-effort publish. No subscribers = silently dropped.
    pub fn publish(&self, ev: IntentEvent) {
        let _ = self.tx.send(ev);
    }

    pub fn subscribe(&self) -> Receiver<IntentEvent> {
        self.tx.subscribe()
    }
}

impl Default for EventBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}
