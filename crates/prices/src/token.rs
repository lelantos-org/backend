//! What a price is about, and what a price is.
//!
//! Both types are provider-agnostic on purpose: they are what every
//! [`crate::PriceProvider`] speaks, so adding a source changes the adapter and
//! nothing a consumer names.

use shared::chain::ChainId;

/// A token as the rest of the workspace identifies it: chain id plus lowercase
/// hex address without a `0x` prefix, matching `hex::encode` of `assets.token`.
///
/// Ordering is by chain then address, which is the order `/v1/prices` sorts its
/// rows into.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenKey {
    pub chain: ChainId,
    pub address: String,
}

impl TokenKey {
    /// The address is normalised here rather than at each call site: it is a
    /// hash key, and one token written two ways must not become two entries in
    /// the cache or two coins in one upstream request.
    ///
    /// Callers pass `hex::encode` of a token column, but a `0x`-prefixed string
    /// is what the same address looks like everywhere else in the system — on
    /// the wire, in a config file, in a log — so it is accepted and stripped
    /// rather than silently keyed as a different token.
    pub fn new(chain: impl Into<ChainId>, address: impl Into<String>) -> Self {
        let address = address.into().to_lowercase();
        Self {
            chain: chain.into(),
            address: address.strip_prefix("0x").unwrap_or(&address).to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenPrice {
    pub price_usd: f64,
    /// `None` when the provider priced the token but reported no decimals:
    /// sufficient for a spot price, insufficient to convert an amount.
    pub decimals: Option<u32>,
    /// Provider's own timestamp for the quote, not our fetch time.
    pub quoted_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_token_written_two_ways_is_one_key() {
        assert_eq!(TokenKey::new(1, "A0B8"), TokenKey::new(1, "a0b8"));
    }

    #[test]
    fn a_prefixed_address_is_the_same_key_as_a_bare_one() {
        assert_eq!(TokenKey::new(1, "0xA0B8"), TokenKey::new(1, "a0b8"));
    }

    #[test]
    fn the_same_address_on_two_chains_is_two_keys() {
        assert_ne!(TokenKey::new(1, "a0b8"), TokenKey::new(8453, "a0b8"));
    }
}
