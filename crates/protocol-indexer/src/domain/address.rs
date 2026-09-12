//! Reading an address back out of a `bytea` column.

use alloy::primitives::Address;

/// Decode `assets.token`, `asset_yield.venue` or any other 20-byte address
/// column.
///
/// `None` when the column is not exactly 20 bytes, which the callers log and
/// skip. Fallible rather than `Address::from_slice`, which *panics* on a
/// wrong-sized slice — and a panic inside a tick kills that worker for every
/// chain, so one malformed row would end the service rather than cost one asset.
pub fn from_column(bytes: &[u8]) -> Option<Address> {
    Address::try_from(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_twenty_byte_column_decodes() {
        assert_eq!(from_column(&[0xab; 20]), Some(Address::repeat_byte(0xab)));
    }

    /// The case that would panic through `Address::from_slice`.
    #[test]
    fn a_wrong_sized_column_is_rejected_rather_than_panicking() {
        assert_eq!(from_column(&[0xab; 19]), None);
        assert_eq!(from_column(&[0xab; 21]), None);
        assert_eq!(from_column(&[]), None);
    }
}
