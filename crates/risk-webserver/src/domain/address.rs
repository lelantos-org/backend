//! Address normalization, the boundary that makes an exact `=` lookup on
//! `screened_addresses.address` correct.
//!
//! Every address entering the service passes through [`normalize`] before it
//! reaches SQL, and the table holds the same normalized form. An error here is a
//! false negative on a sanctioned address, so the rules are strict and the
//! failure mode is `BadRequest` rather than a silent pass.

use crate::domain::error::{AppError, AppResult};

/// Chain-agnostic address, normalized and safe to compare byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedAddress {
    pub chain: String,
    pub address: String,
}

/// Address family whose exact wire format can be validated.
const CHAIN_EVM: &str = "evm";

const MAX_CHAIN_LEN: usize = 16;
const MAX_ADDRESS_LEN: usize = 128;
const EVM_HEX_LEN: usize = 40;

/// Normalize `(chain, raw)` into its canonical stored form.
///
/// - `chain`: trimmed and lowercased; must be `[a-z0-9-]{1,16}`.
/// - `chain == "evm"`: `0x` followed by exactly 40 hex digits, lowercased.
///   EIP-55 checksummed input is accepted and case-folded so a checksummed and a
///   lowercase spelling of the same address screen identically.
/// - any other chain: trimmed, non-empty, at most 128 alphanumeric ASCII
///   characters, with case preserved. This covers base58 and bech32 without
///   verifying their checksums; case is significant in base58, so folding it
///   would merge distinct addresses.
pub fn normalize(chain: &str, raw: &str) -> AppResult<NormalizedAddress> {
    let chain = normalize_chain(chain)?;
    let address = if chain == CHAIN_EVM {
        normalize_evm(raw)?
    } else {
        normalize_opaque(raw)?
    };
    Ok(NormalizedAddress { chain, address })
}

fn normalize_chain(chain: &str) -> AppResult<String> {
    let chain = chain.trim().to_ascii_lowercase();
    if chain.is_empty() || chain.len() > MAX_CHAIN_LEN {
        return Err(AppError::BadRequest(format!(
            "chain must be 1..={MAX_CHAIN_LEN} characters"
        )));
    }
    if !chain
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(AppError::BadRequest(
            "chain must match [a-z0-9-]".to_string(),
        ));
    }
    Ok(chain)
}

fn normalize_evm(raw: &str) -> AppResult<String> {
    let raw = raw.trim();
    let hex = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .ok_or_else(|| AppError::BadRequest("evm address must start with 0x".to_string()))?;
    if hex.len() != EVM_HEX_LEN {
        return Err(AppError::BadRequest(format!(
            "evm address must be 0x followed by {EVM_HEX_LEN} hex digits"
        )));
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::BadRequest(
            "evm address contains a non-hex character".to_string(),
        ));
    }
    Ok(format!("0x{}", hex.to_ascii_lowercase()))
}

fn normalize_opaque(raw: &str) -> AppResult<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(AppError::BadRequest(
            "address must not be empty".to_string(),
        ));
    }
    if raw.len() > MAX_ADDRESS_LEN {
        return Err(AppError::BadRequest(format!(
            "address must be at most {MAX_ADDRESS_LEN} characters"
        )));
    }
    if !raw.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(AppError::BadRequest(
            "address must be ASCII alphanumeric".to_string(),
        ));
    }
    Ok(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVM_MIXED: &str = "0x8589427373D6D84E98730D7795D8f6f8731FDA16";
    const EVM_LOWER: &str = "0x8589427373d6d84e98730d7795d8f6f8731fda16";

    fn is_bad_request(e: AppError) -> bool {
        matches!(e, AppError::BadRequest(_))
    }

    #[test]
    fn test_normalize_evm_mixed_case_lowercases() {
        let got = normalize("evm", EVM_MIXED).unwrap();
        assert_eq!(got.chain, "evm");
        assert_eq!(got.address, EVM_LOWER);
    }

    #[test]
    fn test_normalize_evm_checksummed_and_lowercase_agree() {
        assert_eq!(
            normalize("evm", EVM_MIXED).unwrap(),
            normalize("evm", EVM_LOWER).unwrap()
        );
    }

    #[test]
    fn test_normalize_evm_trims_and_lowercases_chain() {
        let got = normalize("  EVM ", EVM_LOWER).unwrap();
        assert_eq!(got.chain, "evm");
    }

    #[test]
    fn test_normalize_evm_bad_length_is_bad_request() {
        let err = normalize("evm", "0xdeadbeef").unwrap_err();
        assert!(is_bad_request(err));
        let err = normalize("evm", &format!("{EVM_LOWER}00")).unwrap_err();
        assert!(is_bad_request(err));
    }

    #[test]
    fn test_normalize_evm_missing_prefix_is_bad_request() {
        let err = normalize("evm", &EVM_LOWER[2..]).unwrap_err();
        assert!(is_bad_request(err));
    }

    #[test]
    fn test_normalize_evm_non_hex_is_bad_request() {
        let mut bad = EVM_LOWER.to_string();
        bad.replace_range(5..6, "z");
        let err = normalize("evm", &bad).unwrap_err();
        assert!(is_bad_request(err));
    }

    #[test]
    fn test_normalize_unknown_chain_preserves_case() {
        let got = normalize("btc", "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2").unwrap();
        assert_eq!(got.chain, "btc");
        assert_eq!(got.address, "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2");
    }

    #[test]
    fn test_normalize_rejects_empty_address() {
        assert!(is_bad_request(normalize("btc", "   ").unwrap_err()));
        assert!(is_bad_request(normalize("evm", "").unwrap_err()));
    }

    #[test]
    fn test_normalize_rejects_oversize_address() {
        let long = "a".repeat(MAX_ADDRESS_LEN + 1);
        assert!(is_bad_request(normalize("btc", &long).unwrap_err()));
    }

    #[test]
    fn test_normalize_rejects_non_alphanumeric_address() {
        assert!(is_bad_request(normalize("btc", "abc-def").unwrap_err()));
    }

    #[test]
    fn test_normalize_rejects_bad_chain() {
        assert!(is_bad_request(normalize("", EVM_LOWER).unwrap_err()));
        assert!(is_bad_request(normalize("e v m", EVM_LOWER).unwrap_err()));
        assert!(is_bad_request(
            normalize(&"a".repeat(MAX_CHAIN_LEN + 1), "abc").unwrap_err()
        ));
    }
}
