use crate::domain::risk::RiskLevel;
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

/// One row of the screening list, as stored.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntryOut {
    pub chain: String,
    pub address: String,
    pub risk: RiskLevel,
    pub source: String,
    pub reason: Option<String>,
    pub added_at: DateTime<Utc>,
}
