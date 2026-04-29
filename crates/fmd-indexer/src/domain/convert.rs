use ark_ed_on_bn254::Fq;
use ark_ff::PrimeField;
use bigdecimal::BigDecimal;
use bigdecimal::num_bigint::{BigInt, Sign};

pub fn u256_to_bigdecimal(v: alloy::primitives::U256) -> BigDecimal {
    let bytes = v.to_be_bytes::<32>();
    BigDecimal::from(BigInt::from_bytes_be(Sign::Plus, &bytes))
}

pub fn bigdec_to_fq(v: &BigDecimal) -> Fq {
    let (bi, _scale) = v.as_bigint_and_exponent();
    let (sign, bytes) = bi.to_bytes_be();
    let mut adjusted = Fq::from_be_bytes_mod_order(&bytes);
    if sign == Sign::Minus {
        adjusted = -adjusted;
    }
    adjusted
}

pub fn clue_bits_be(ciphertext: &[u8]) -> Option<u16> {
    if ciphertext.len() < 2 {
        return None;
    }
    Some(u16::from_be_bytes([ciphertext[0], ciphertext[1]]))
}
