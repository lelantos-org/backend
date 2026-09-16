//! USD spot prices for ERC20 tokens, shared by every service that reports them.
//!
//! Layering: a leaf library over `shared`. It imports no other internal crate,
//! in particular not `database`, since a price is keyed by chain id and token
//! address rather than by a stored row.
//!
//! ```text
//! token      TokenKey / TokenPrice   what every provider speaks
//! providers  PriceProvider           the price-source interface, and
//!            DefiLlama, …            one module per upstream
//! service    PriceService            cache + ordered provider fallback
//! convert    to_usd                  base units -> dollars
//! ```
//!
//! Consumers hold a [`PriceService`] and never name a provider past the line
//! that constructs one, so adding a source is a new module under `providers`
//! plus one entry in the vector handed to [`PriceService::new`].

pub mod convert;
pub mod providers;
pub mod service;
pub mod token;

pub use convert::to_usd;
pub use providers::{DefiLlama, PriceProvider};
pub use service::PriceService;
pub use token::{TokenKey, TokenPrice};
