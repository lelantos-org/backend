//! Solidity event ABI bindings + decode.
//!
//! Pure data crate. May import: `shared`, alloy. Must NOT import:
//! `database`, `fmd-crypto`, any binary or service crate.

pub mod abi;
pub mod decode;

pub use decode::{DecodeError, DecodedEvent, decode};
