//! Layer 2: the outside world — the RPC transport, ABI bindings, the pool's
//! view calls and wire parsing. No orchestration.

pub mod abi;
pub mod calldata;
pub mod masp;
pub mod parse;
pub mod rpc;
