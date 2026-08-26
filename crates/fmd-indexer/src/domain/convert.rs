use ark_ed_on_bn254::Fq;
use ark_ff::PrimeField;
use bigdecimal::BigDecimal;
use bigdecimal::num_bigint::{BigInt, Sign};

pub fn u256_to_bigdecimal(v: alloy::primitives::U256) -> BigDecimal {
    let bytes = v.to_be_bytes::<32>();
    BigDecimal::from(BigInt::from_bytes_be(Sign::Plus, &bytes))
}

/// Reinterpret a `NUMERIC(78, 0)` column as a field element.
///
/// The exponent is dropped: every column read through here is declared with
/// scale 0, so the `BigDecimal` is a plain integer. A non-zero scale would yield
/// the wrong element, which the unit test below pins.
pub fn bigdec_to_fq(v: &BigDecimal) -> Fq {
    let (bi, _scale) = v.as_bigint_and_exponent();
    let (sign, bytes) = bi.to_bytes_be();
    let mut adjusted = Fq::from_be_bytes_mod_order(&bytes);
    if sign == Sign::Minus {
        adjusted = -adjusted;
    }
    adjusted
}

/// Read the packed FMD clue bits from a ciphertext's 2-byte prefix.
///
/// Big-endian, matching what the contract writes. `fmd_crypto::filter` documents
/// its `clue_bits` argument as little-endian, which refers to the bit order
/// inside the u16 rather than the byte order on the wire; the two are consistent
/// and `tests/fixture_replay.rs` pins the round trip.
pub fn clue_bits_be(ciphertext: &[u8]) -> Option<u16> {
    if ciphertext.len() < 2 {
        return None;
    }
    Some(u16::from_be_bytes([ciphertext[0], ciphertext[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ff::BigInteger;

    fn fq_dec(v: &Fq) -> String {
        BigInt::from_bytes_be(Sign::Plus, &v.into_bigint().to_bytes_be()).to_string()
    }

    #[test]
    fn bigdec_to_fq_reads_scale_zero_integers() {
        // The shape every `NUMERIC(78, 0)` read takes. If diesel returned a
        // scaled representation of the same value (1E+2 for 100), the dropped
        // exponent would change the answer.
        for dec in ["0", "1", "100", "123456789012345678901234567890"] {
            let v = BigDecimal::parse_bytes(dec.as_bytes(), 10).unwrap();
            assert_eq!(v.as_bigint_and_exponent().1, 0, "{dec} must be scale 0");
            assert_eq!(fq_dec(&bigdec_to_fq(&v)), dec);
        }
    }

    #[test]
    fn clue_bits_be_reads_the_two_byte_prefix() {
        assert_eq!(clue_bits_be(&[0x00, 0x07, 0xff]), Some(7));
        assert_eq!(clue_bits_be(&[0x01, 0x00]), Some(256));
        assert_eq!(clue_bits_be(&[0x01]), None);
        assert_eq!(clue_bits_be(&[]), None);
    }
}
