//! Solidity ABI bindings for the chain, plus log decode and the `NUMERIC`
//! conversions those values are stored through.
//!
//! Pure data by default. May import: `shared`, alloy. Must NOT import:
//! `database`, `common-crypto`, any binary or service crate.
//!
//! The `rpc` feature is the one exception to "no IO": it adds the shared
//! JSON-RPC transport ([`rpc::RpcEndpoint`]), which lives here because it is
//! the chain-facing layer and had otherwise been copied between services. It is
//! off by default, so the indexers that link this crate for the ABI alone do
//! not pull a provider stack in with it.

pub mod abi;
pub mod decode;
pub mod numeric;
#[cfg(feature = "rpc")]
pub mod rpc;

pub use decode::{DecodeError, DecodedEvent, decode};
