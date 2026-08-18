use crate::domain::error::{AppError, AppResult};
use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

/// BN254 scalar field order — the modulus every in-circuit signal lives under,
/// and the one `fmd_crypto::poseidon` (over `ark_ed_on_bn254::Fq`) enforces.
pub static BN254_R: LazyLock<U256> = LazyLock::new(|| {
    U256::from_str_radix(
        "21888242871839275222246405745257275088548364400416034343698204186575808495617",
        10,
    )
    .expect("BN254 modulus literal")
});

/// Parse a `0x`-hex or decimal integer. Accepts either case of the prefix, and
/// strips it exactly once — `"0x0x12"` is a malformed input, not `0x12`.
pub fn parse_u256(s: &str) -> AppResult<U256> {
    let (body, radix) = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => (hex, 16),
        None => (s, 10),
    };
    U256::from_str_radix(body, radix)
        .map_err(|e| AppError::BadRequest(format!("u256 parse: {}", e)))
}

pub fn parse_b32(s: &str) -> AppResult<FixedBytes<32>> {
    let v = parse_u256(s)?;
    Ok(FixedBytes::<32>::from(v.to_be_bytes::<32>()))
}

/// Why a value cannot serve as an in-circuit field element.
///
/// Deliberately carries no message: the caller knows which input it was
/// reading, and building that label eagerly would allocate on every element of
/// every valid payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotAField {
    /// Not an integer literal at all.
    Malformed,
    /// An integer, but at or above the BN254 scalar modulus.
    NonCanonical,
}

impl NotAField {
    fn reason(self) -> &'static str {
        match self {
            NotAField::Malformed => "is not a decimal or 0x-hex integer",
            NotAField::NonCanonical => {
                "is not a canonical field element (must be below the BN254 scalar modulus)"
            }
        }
    }
}

/// Core field parse. Rejects anything at or above the BN254 scalar modulus:
/// such a value is not a field element at all, `fmd_crypto`'s Poseidon refuses
/// it, and the contract's coefficient range check reverts on it. Left
/// unchecked, a non-canonical `outCm` reaches [`crate::services::tree`] and
/// fails *between* two speculative leaf inserts, which is how a single request
/// could permanently desync a chain's mirror.
fn field_bytes(s: &str) -> Result<FixedBytes<32>, NotAField> {
    let v = parse_u256(s).map_err(|_| NotAField::Malformed)?;
    if v >= *BN254_R {
        return Err(NotAField::NonCanonical);
    }
    Ok(FixedBytes::<32>::from(v.to_be_bytes::<32>()))
}

/// Where a field element came from, for error messages.
///
/// Rendered only when a parse fails, so a valid payload never pays for the
/// label — building `format!("nullifier[{i}]")` eagerly would allocate once per
/// element of every request.
#[derive(Debug, Clone, Copy)]
pub enum FieldRef<'a> {
    /// A single named input, e.g. `pubInputs.merkleRoot`.
    Named(&'a str),
    /// One slot of a named array, e.g. `pubInputs.nullifier[2]`.
    Index(&'a str, usize),
    /// One coordinate of a point in a named array, e.g. `pubInputs.inCv[1].y`.
    Coord(&'a str, usize, &'a str),
}

impl fmt::Display for FieldRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            FieldRef::Named(name) => write!(f, "{name}"),
            FieldRef::Index(array, i) => write!(f, "{array}[{i}]"),
            FieldRef::Coord(array, i, coord) => write!(f, "{array}[{i}].{coord}"),
        }
    }
}

/// Parse a value used as an in-circuit field element. `at` names the input so
/// the caller can fix their payload.
pub fn parse_field(s: &str, at: FieldRef<'_>) -> AppResult<FixedBytes<32>> {
    field_bytes(s).map_err(|e| AppError::BadRequest(format!("{at} {}", e.reason())))
}

pub fn parse_address(s: &str) -> AppResult<Address> {
    Address::from_str(s).map_err(|e| AppError::BadRequest(format!("address parse: {}", e)))
}

/// Decode an optionally `0x`-prefixed hex string into raw bytes. `field` is
/// used only to build the error message.
pub fn parse_hex_bytes(s: &str, field: &'static str) -> AppResult<Bytes> {
    let body = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    let raw = hex::decode(body).map_err(|e| AppError::BadRequest(format!("{field} hex: {e}")))?;
    Ok(Bytes::from(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(v: &U256) -> String {
        v.to_string()
    }

    #[test]
    fn parses_decimal_and_either_hex_prefix() {
        assert_eq!(parse_u256("18").unwrap(), U256::from(18u8));
        assert_eq!(parse_u256("0x12").unwrap(), U256::from(18u8));
        assert_eq!(parse_u256("0X12").unwrap(), U256::from(18u8));
    }

    /// `trim_start_matches` stripped every repetition, so a doubled prefix
    /// silently parsed as a valid value instead of being rejected.
    #[test]
    fn a_doubled_prefix_is_malformed_not_stripped_twice() {
        assert!(parse_u256("0x0x12").is_err());
    }

    #[test]
    fn accepts_a_canonical_field_element() {
        let max = *BN254_R - U256::from(1u8);
        assert_eq!(
            parse_field(&dec(&max), FieldRef::Named("outCm")).unwrap(),
            FixedBytes::<32>::from(max.to_be_bytes::<32>())
        );
    }

    /// The modulus itself and everything above it is not a field element.
    /// Poseidon rejects these, and letting one through is what corrupts the
    /// tree mirror mid-batch.
    #[test]
    fn rejects_non_canonical_field_elements() {
        for v in [*BN254_R, *BN254_R + U256::from(1u8), U256::MAX] {
            assert_eq!(field_bytes(&dec(&v)), Err(NotAField::NonCanonical));
            let err = parse_field(&dec(&v), FieldRef::Named("outCm")).unwrap_err();
            assert!(matches!(err, AppError::BadRequest(_)), "got {err}");
            assert!(err.to_string().contains("outCm"), "got {err}");
        }
    }

    /// A value that is not an integer at all reports that, rather than being
    /// reported as out of range.
    #[test]
    fn distinguishes_malformed_from_non_canonical() {
        assert_eq!(field_bytes("not-a-number"), Err(NotAField::Malformed));
        let err = parse_field("zzz", FieldRef::Named("outCm")).unwrap_err();
        assert!(err.to_string().contains("integer"), "got {err}");
    }

    /// Naming the exact slot is what makes a rejection actionable when one
    /// element of a three-element array — or one coordinate of a point — is bad.
    #[test]
    fn a_rejection_names_the_exact_slot() {
        let bad = dec(&U256::MAX);
        for (at, expected) in [
            (
                FieldRef::Named("pubInputs.merkleRoot"),
                "pubInputs.merkleRoot",
            ),
            (
                FieldRef::Index("pubInputs.nullifier", 2),
                "pubInputs.nullifier[2]",
            ),
            (
                FieldRef::Coord("pubInputs.inCv", 1, "y"),
                "pubInputs.inCv[1].y",
            ),
        ] {
            let err = parse_field(&bad, at).unwrap_err();
            assert!(err.to_string().contains(expected), "got {err}");
        }
    }

    /// `parse_b32` still admits the full 256-bit range: it backs values the
    /// contract range-checks itself, not ones Poseidon will hash.
    #[test]
    fn parse_b32_still_accepts_the_full_width() {
        assert!(parse_b32(&dec(&U256::MAX)).is_ok());
    }

    #[test]
    fn hex_bytes_accept_either_prefix_case() {
        assert_eq!(
            parse_hex_bytes("0xdead", "route").unwrap().to_vec(),
            vec![0xde, 0xad]
        );
        assert_eq!(
            parse_hex_bytes("0Xdead", "route").unwrap().to_vec(),
            vec![0xde, 0xad]
        );
        assert_eq!(
            parse_hex_bytes("dead", "route").unwrap().to_vec(),
            vec![0xde, 0xad]
        );
    }
}
