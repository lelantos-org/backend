//! Outbound integrations.
//!
//! The DefiLlama price client lives in the `prices` crate — the relayer serves
//! it too — and is re-exported here so the services that decorate rows with USD
//! keep naming it as an adapter.

pub use prices::{PriceClient, TokenKey, TokenPrice};
