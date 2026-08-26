//! bech32m shielded payment address.
//!
//! Rust mirror of `sdk/src/keys/address.ts`, and the only place this crate
//! interprets one. It decodes but never encodes: the relayer publishes the
//! operator-supplied string verbatim and uses the decoded parts only to check
//! that string against the viewing key configured beside it.
//!
//! ```text
//! HRP     "lelantos"
//! payload pk_d (32 B, Baby-Jubjub packed  — ECDH target)
//!      || pk   (32 B, little-endian field — note-commitment binding)
//!      || ck   (32 B, Baby-Jubjub packed  — FMD clue key)
//! ```
//!
//! The HRP carries the format version, so a future layout change fails to
//! decode rather than being misread.

use crate::domain::error::{AppError, AppResult};
use bech32::primitives::decode::CheckedHrpstring;
use bech32::{Bech32m, Hrp};
use fmd_crypto::clue::unpack_subgroup;
use fmd_crypto::tree::Field;

pub const ADDRESS_HRP: &str = "lelantos";
const FIELD_BYTES: usize = 32;
const PAYLOAD_LEN: usize = 3 * FIELD_BYTES;

/// The public halves of a shielded identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShieldedAddress {
    /// ECDH target, packed. Kept in wire form: it is only ever compared
    /// against a freshly packed point, never used as a field element.
    pub pk_d_packed: [u8; FIELD_BYTES],
    /// Note-commitment binding key, big-endian — this crate's field
    /// convention, not the address payload's little-endian one.
    pub pk: Field,
    /// FMD clue key, packed. Unused by the fee path, but decoded and validated so
    /// a malformed address is caught at boot rather than at first use.
    pub ck_packed: [u8; FIELD_BYTES],
}

/// Decode and fully validate an address.
///
/// Both point slots are checked on-curve, in the prime-order subgroup and
/// non-identity, matching `unpackChecked` on the SDK side. A field scalar pasted
/// into a point slot must fail here rather than yield an address nobody can pay.
pub fn decode(addr: &str) -> AppResult<ShieldedAddress> {
    let parsed = CheckedHrpstring::new::<Bech32m>(addr)
        .map_err(|e| bad(format!("not a bech32m string: {e}")))?;

    let hrp = parsed.hrp();
    if hrp != Hrp::parse_unchecked(ADDRESS_HRP) {
        return Err(bad(format!("bad HRP {hrp}, expected {ADDRESS_HRP}")));
    }

    let payload: Vec<u8> = parsed.byte_iter().collect();
    if payload.len() != PAYLOAD_LEN {
        return Err(bad(format!(
            "bad payload length {}, expected {PAYLOAD_LEN}",
            payload.len()
        )));
    }

    let pk_d_packed = slot(&payload, 0);
    let ck_packed = slot(&payload, 2);
    check_point(&pk_d_packed, "pk_d")?;
    check_point(&ck_packed, "ck")?;

    // The payload spells `pk` little-endian; everything downstream of here is
    // big-endian.
    let mut pk = slot(&payload, 1);
    pk.reverse();

    Ok(ShieldedAddress {
        pk_d_packed,
        pk,
        ck_packed,
    })
}

fn slot(payload: &[u8], index: usize) -> [u8; FIELD_BYTES] {
    let start = index * FIELD_BYTES;
    payload[start..start + FIELD_BYTES]
        .try_into()
        .expect("payload length checked above")
}

fn check_point(packed: &[u8; FIELD_BYTES], name: &str) -> AppResult<()> {
    unpack_subgroup(packed)
        .map(|_| ())
        .map_err(|e| bad(format!("{name}: {e}")))
}

fn bad(detail: String) -> AppError {
    AppError::Internal(format!("shielded address: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Emitted by `sdk/src/keys/address.ts :: encodeAddress` for the spending key
    /// at seed 7777, the same key the `fmd-crypto` note vectors use, so `pk` here
    /// is checkable against those.
    const VALID: &str = include_str!("../../tests/vectors/shielded-address.txt");

    fn valid() -> &'static str {
        VALID.trim()
    }

    #[test]
    fn decodes_a_wallet_generated_address() {
        let a = decode(valid()).expect("decodes");
        assert_eq!(
            hex::encode(a.pk),
            // pk for seed 7777, big-endian; matches `pkDec` in
            // `crates/fmd-crypto/tests/vectors/note-parity.json`.
            "0c70606823cfb3c8f358f6c1b7faf360ee0fddd827b4493f83b530ee8e41c053"
        );
    }

    #[test]
    fn rejects_a_foreign_hrp() {
        let swapped = valid().replacen(ADDRESS_HRP, "lelantox", 1);
        assert!(decode(&swapped).is_err());
    }

    #[test]
    fn rejects_a_corrupted_checksum() {
        let mut s = valid().to_string();
        let last = s.pop().expect("non-empty");
        s.push(if last == 'q' { 'p' } else { 'q' });
        assert!(decode(&s).is_err());
    }

    #[test]
    fn rejects_a_non_bech32m_string() {
        assert!(decode("not-an-address").is_err());
        assert!(decode("").is_err());
    }
}
