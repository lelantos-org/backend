//! Outbound integrations.
//!
//! Prices live in the `prices` crate, shared with the relayer, and are
//! re-exported here so the services that decorate rows with USD refer to them as
//! an adapter. Which upstream answers is that crate's business: past `main`,
//! where the provider list is built, this crate names only [`PriceService`].

pub use prices::{DefiLlama, PriceService, TokenKey, TokenPrice};
