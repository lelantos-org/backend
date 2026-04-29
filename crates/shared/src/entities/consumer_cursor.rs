use crate::chain::ChainId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsumerCursor {
    pub name: String,
    pub chain_id: ChainId,
    pub last_event_id: i64,
    pub last_block_number: i64,
    pub updated_at: DateTime<Utc>,
}
