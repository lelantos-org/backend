//! Layer 2: the outside world — the RPC transport, ABI bindings, wire parsing
//! and the `NUMERIC` columns the indexer writes. No orchestration.

pub mod abi;
pub mod calldata;
pub mod numeric;
pub mod parse;
pub mod rpc;
