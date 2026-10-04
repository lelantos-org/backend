//! Tree leaves. A leaf is a note commitment: a spend's `out_cm` as it stands,
//! and for a deposit the hash the batch circuit builds from the escrow; see
//! `PaddedBatch::leaves`.

use crate::domain::error::{AppError, AppResult};
use crate::domain::field::is_canonical;
use crypto::tree::Field;

/// Refuse a leaf at or above the field modulus. The pool cannot insert one, and
/// the mirror's hashes would fold it reduced.
pub(super) fn check_canonical(leaf: &Field) -> AppResult<()> {
    if !is_canonical(leaf) {
        return Err(AppError::Internal(
            "leaf is not a canonical field element".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::field::BN254_R;
    use alloy::primitives::U256;

    #[test]
    fn a_non_canonical_leaf_is_refused() {
        assert!(check_canonical(&BN254_R.to_be_bytes()).is_err());
        assert!(check_canonical(&(*BN254_R - U256::from(1u8)).to_be_bytes()).is_ok());
    }
}
