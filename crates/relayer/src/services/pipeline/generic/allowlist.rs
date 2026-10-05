//! `generic_allowed_calls`: the `(target, selector)` pairs a generic intent may
//! call.

use crate::adapters::abi::IGenericCallWrapper;
use crate::app::config::AllowedCallCfg;
use crate::domain::error::{AppError, AppResult};
use alloy::primitives::{Address, Selector};
use std::collections::HashSet;
use std::str::FromStr;

/// One chain's allowlist, parsed.
pub type AllowedCalls = HashSet<(Address, Selector)>;

pub fn parse_allowed_calls(cfg: &[AllowedCallCfg]) -> AppResult<AllowedCalls> {
    cfg.iter()
        .map(|c| {
            let target = Address::from_str(&c.target)
                .map_err(|e| AppError::Internal(format!("target {}: {e}", c.target)))?;
            let selector = Selector::from_str(&c.selector)
                .map_err(|e| AppError::Internal(format!("selector {}: {e}", c.selector)))?;
            Ok((target, selector))
        })
        .collect()
}

/// Every call must be on the allowlist. Calldata shorter than a selector names
/// no function, so it matches nothing.
pub(super) fn check_calls(
    calls: &[IGenericCallWrapper::Call],
    allowed: &AllowedCalls,
) -> AppResult<()> {
    for (i, call) in calls.iter().enumerate() {
        let Some(selector) = call.data.get(..4).map(Selector::from_slice) else {
            return Err(AppError::BadRequest(format!(
                "generic.calls[{i}].data is shorter than a 4-byte selector"
            )));
        };
        if !allowed.contains(&(call.target, selector)) {
            return Err(AppError::BadRequest(format!(
                "generic.calls[{i}] ({} {selector}) is not a call this relayer relays",
                call.target
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = "0x000000000000000000000000000000000000dead";

    fn entry(target: &str, selector: &str) -> AllowedCallCfg {
        AllowedCallCfg {
            target: target.into(),
            selector: selector.into(),
        }
    }

    /// A repeated pair is listed once, and the selector's `0x` is optional.
    #[test]
    fn allowed_calls_parse_from_config() {
        let other = Address::repeat_byte(0xAA);
        let parsed = parse_allowed_calls(&[
            entry(&other.to_string(), "0xdeadbeef"),
            entry(&other.to_string(), "0xdeadbeef"),
            entry(TARGET, "01020304"),
        ])
        .unwrap();
        assert_eq!(
            parsed,
            HashSet::from([
                (other, Selector::from([0xde, 0xad, 0xbe, 0xef])),
                (
                    Address::from_str(TARGET).unwrap(),
                    Selector::from([1, 2, 3, 4])
                ),
            ])
        );
    }

    #[test]
    fn allowed_calls_refuse_a_malformed_entry() {
        for (target, selector) in [
            (TARGET, "0xdeadbe"),
            (TARGET, "0xdeadbeef01"),
            (TARGET, "transfer"),
            ("0x1234", "0xdeadbeef"),
        ] {
            let err = parse_allowed_calls(&[entry(target, selector)]).unwrap_err();
            assert!(
                matches!(err, AppError::Internal(_)),
                "{target} {selector}: {err}"
            );
        }
    }
}
