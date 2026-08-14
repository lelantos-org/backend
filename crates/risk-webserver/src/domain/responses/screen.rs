use crate::domain::risk::RiskLevel;
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

/// One listing that matched the screened address.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MatchOut {
    pub source: String,
    pub risk: RiskLevel,
    pub reason: Option<String>,
    pub added_at: DateTime<Utc>,
}

/// Verdict for one address.
///
/// `address` echoes the *normalized* form, not what the caller sent, so a
/// caller can tell which key the answer is about.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScreenOut {
    pub chain: String,
    pub address: String,
    /// Highest risk across `matches`, or `none` when there are no matches.
    pub risk: RiskLevel,
    /// Derived from `risk`; the authoritative field for callers to branch on.
    pub blocked: bool,
    pub matches: Vec<MatchOut>,
}
