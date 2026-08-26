use crate::domain::error::{AppError, AppResult};
use serde::Serialize;
use utoipa::ToSchema;

/// Risk verdict for one address.
///
/// `None` is not a stored value; it is the verdict for an address with no rows.
/// The stored levels are constrained by `screened_addresses_risk_check` in the
/// migration, so `from_db` fails only if that constraint is bypassed.
///
/// Declaration order is the ascending ranking that `Ord` derives from, and the
/// screen result is `max()` over every matching row. `None` must stay first so
/// an unlisted address ranks below every listed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    None,
    Low,
    Medium,
    High,
    Banned,
}

impl RiskLevel {
    /// The block policy. Callers key off `blocked` rather than re-deriving it
    /// from `risk`.
    pub fn blocked(self) -> bool {
        matches!(self, RiskLevel::Banned | RiskLevel::High)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RiskLevel::Banned => "banned",
            RiskLevel::High => "high",
            RiskLevel::Medium => "medium",
            RiskLevel::Low => "low",
            RiskLevel::None => "none",
        }
    }

    /// Parse a `screened_addresses.risk` value. `none` is rejected because it is
    /// a computed verdict rather than a stored row.
    pub fn from_db(s: &str) -> AppResult<Self> {
        match s {
            "banned" => Ok(RiskLevel::Banned),
            "high" => Ok(RiskLevel::High),
            "medium" => Ok(RiskLevel::Medium),
            "low" => Ok(RiskLevel::Low),
            other => Err(AppError::Internal(format!(
                "unknown risk level in screened_addresses: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_risk_level_ordering_banned_is_max() {
        let levels = [
            RiskLevel::None,
            RiskLevel::Low,
            RiskLevel::Banned,
            RiskLevel::Medium,
        ];
        assert_eq!(*levels.iter().max().unwrap(), RiskLevel::Banned);
        assert_eq!(*levels.iter().min().unwrap(), RiskLevel::None);
        assert!(RiskLevel::High > RiskLevel::Medium);
    }

    #[test]
    fn test_blocked_covers_banned_and_high_only() {
        assert!(RiskLevel::Banned.blocked());
        assert!(RiskLevel::High.blocked());
        assert!(!RiskLevel::Medium.blocked());
        assert!(!RiskLevel::Low.blocked());
        assert!(!RiskLevel::None.blocked());
    }

    #[test]
    fn test_from_db_rejects_none_and_unknown() {
        assert!(RiskLevel::from_db("none").is_err());
        assert!(RiskLevel::from_db("severe").is_err());
        assert_eq!(RiskLevel::from_db("banned").unwrap(), RiskLevel::Banned);
    }

    #[test]
    fn test_serialize_is_lowercase() {
        let json = serde_json::to_string(&RiskLevel::Banned).unwrap();
        assert_eq!(json, "\"banned\"");
    }
}
