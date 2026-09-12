//! Price sources.
//!
//! One module per upstream, each implementing [`crate::PriceProvider`]. Nothing
//! outside a module here names the provider's wire format.

pub mod defillama;

pub use defillama::DefiLlama;
