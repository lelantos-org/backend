use crate::chain::ChainId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Match {
    pub subscription_id: i64,
    pub note_id: i64,
    pub chain_id: ChainId,
    pub matched_at: DateTime<Utc>,
}
