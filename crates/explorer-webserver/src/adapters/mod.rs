//! Outbound integrations.
//!
//! The DefiLlama price client lives in the `prices` crate, shared with the
//! relayer, and is re-exported here so the services that decorate rows with USD
//! refer to it as an adapter.

pub use prices::{PriceClient, TokenKey, TokenPrice};
