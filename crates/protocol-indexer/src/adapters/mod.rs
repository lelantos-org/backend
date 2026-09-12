//! Chain reads. One module per contract surface this crate calls.

pub mod erc20;
pub mod masp;

pub use erc20::{DynTokenMetadata, HttpTokenMetadata, TokenMetadata};
