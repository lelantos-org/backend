use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: i64,
    pub detection_key: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub active: bool,
}
