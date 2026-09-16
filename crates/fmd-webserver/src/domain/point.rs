//! Baby-Jubjub point compression for the ephemeral public keys the note feeds
//! serve.

use crate::domain::error::{AppError, AppResult};
use crate::domain::field::bigdec_to_field;
use crypto::tree::Field;

/// The Baby-Jubjub base field modulus, `q`, in decimal.
///
/// Only [`Q_HALF_BE`] derives from it, and only in a test, so that constant is
/// checked rather than trusted as transcribed.
#[cfg(test)]
const Q_DEC: &str = "21888242871839275222246405745257275088548364400416034343698204186575808495617";

/// Half the Baby-Jubjub base field modulus, `(q-1)/2`, big-endian.
///
/// circomlibjs treats `x` as negative when `x > (q-1)/2`, and that predicate is
/// the packed point's sign bit. Comparing fixed-width big-endian arrays is
/// equivalent to comparing the numbers, so a plain `>` suffices below.
///
/// Pinned by `q_half_matches_the_modulus`: a mis-transcribed digit here would
/// flip the sign bit for a band of `x` values with no other symptom.
const Q_HALF_BE: Field = [
    0x18, 0x32, 0x27, 0x39, 0x70, 0x98, 0xd0, 0x14, 0xdc, 0x28, 0x22, 0xdb, 0x40, 0xc0, 0xac, 0x2e,
    0x94, 0x19, 0xf4, 0x24, 0x3c, 0xdc, 0xb8, 0x48, 0xa1, 0xf0, 0xfa, 0xc9, 0xf8, 0x00, 0x00, 0x00,
];

/// Pack a Baby-Jubjub point into the 32 bytes circomlibjs `babyJub.packPoint`
/// produces: `y` little-endian, with the high bit of the last byte set when
/// `x > (q-1)/2`.
///
/// `x` is recoverable from `y` and that one bit, so only `y` travels, halving
/// what an ephemeral public key costs on a feed every wallet downloads in full.
///
/// Little-endian, unlike every other field here. This is not a number the client
/// parses but the exact byte string `decryptNote` expects as `epk`, so it mirrors
/// `sdk/wasm/jubjub/src/curve.rs::compress` byte for byte. Serving it big-endian
/// would decode to a different, valid-looking point.
pub fn pack_point(x: &bigdecimal::BigDecimal, y: &bigdecimal::BigDecimal) -> AppResult<[u8; 32]> {
    let x_be = coordinate(x, "x")?;
    // `bigdec_to_field` yields big-endian; the packed form is little-endian.
    let mut packed = coordinate(y, "y")?;
    packed.reverse();
    if x_be > Q_HALF_BE {
        packed[31] |= 0x80;
    }
    Ok(packed)
}

/// [`bigdec_to_field`] naming the coordinate it rejected.
///
/// Both coordinates fail the same way, so without the name a bad row reports only
/// that some field element was malformed.
fn coordinate(v: &bigdecimal::BigDecimal, name: &str) -> AppResult<Field> {
    bigdec_to_field(v).map_err(|e| AppError::Internal(format!("ephemeral pubkey {name}: {e}")))
}

/// [`pack_point`] straight to the wire form.
///
/// Bare hex rather than `0x`-prefixed: the prefix disambiguates numbers for the
/// SDK's decimal-or-hex decoder, while this is a byte string routed through the
/// same path as `ciphertextHex`.
pub fn pack_point_hex(x: &bigdecimal::BigDecimal, y: &bigdecimal::BigDecimal) -> AppResult<String> {
    Ok(hex::encode(pack_point(x, y)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> bigdecimal::BigDecimal {
        bigdecimal::BigDecimal::from_str(s).unwrap()
    }

    /// circomlibjs `Base8`, the generator every SDK point derives from.
    fn base8() -> (bigdecimal::BigDecimal, bigdecimal::BigDecimal) {
        (
            dec("5299619240641551281634865583518297030282874472190772894086521144482721001553"),
            dec("16950150798460657717958625567821834550301663161624707787222815936182638968203"),
        )
    }

    #[test]
    fn packs_a_point_as_little_endian_y() {
        // Byte-for-byte `babyJub.packPoint(Base8)`. `Base8.x < (q-1)/2`, so the
        // sign bit stays clear and this is plain little-endian `y`.
        let (x, y) = base8();
        assert_eq!(
            pack_point_hex(&x, &y).unwrap(),
            "8b7d2d877a253c4b7733e1b91f05e0fcedf96bd11c2e572549b2a0f703727925"
        );
    }

    #[test]
    fn q_half_matches_the_modulus() {
        // Derives `(q-1)/2` rather than trusting the transcription, so a bad
        // digit is reported as such instead of surfacing indirectly.
        let want = bigdec_to_field(&((dec(Q_DEC) - dec("1")) / dec("2"))).unwrap();
        assert_eq!(Q_HALF_BE, want);
    }

    #[test]
    fn sets_the_sign_bit_for_a_negative_x() {
        // Negating x flips only the sign bit: 0x25 -> 0xa5. Inverting this yields
        // a valid-looking point on the wrong branch, which no length or format
        // check would catch.
        let (x, y) = base8();
        let packed = pack_point_hex(&(dec(Q_DEC) - x), &y).unwrap();
        assert_eq!(
            packed,
            "8b7d2d877a253c4b7733e1b91f05e0fcedf96bd11c2e572549b2a0f7037279a5"
        );
    }

    #[test]
    fn packs_the_two_x_parities_to_the_same_y_bytes() {
        let (x, y) = base8();
        let pos = pack_point(&x, &y).unwrap();
        let neg = pack_point(&(dec(Q_DEC) - x), &y).unwrap();
        assert_eq!(pos[..31], neg[..31], "only the last byte may differ");
        assert_eq!(pos[31] | 0x80, neg[31]);
    }

    #[test]
    fn treats_exactly_half_the_modulus_as_non_negative() {
        // circomlibjs uses a strict `x > (q-1)/2`. An `>=` here would flip the
        // sign bit for one specific x and corrupt only those notes.
        let q_half =
            dec("10944121435919637611123202872628637544274182200208017171849102093287904247808");
        let (_, y) = base8();
        assert_eq!(pack_point(&q_half, &y).unwrap()[31] & 0x80, 0);
        assert_eq!(
            pack_point(&(q_half + dec("1")), &y).unwrap()[31] & 0x80,
            0x80
        );
    }

    #[test]
    fn names_the_coordinate_it_rejects() {
        let (x, y) = base8();
        let on_x = pack_point(&dec("-1"), &y).unwrap_err().to_string();
        let on_y = pack_point(&x, &dec("-1")).unwrap_err().to_string();
        assert!(on_x.contains("ephemeral pubkey x"), "{on_x}");
        assert!(on_y.contains("ephemeral pubkey y"), "{on_y}");
    }
}
