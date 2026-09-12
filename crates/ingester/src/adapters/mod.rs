//! Outbound talk to external systems. Holds no domain types of its own beyond
//! what the provider hands back.

pub mod rpc;

pub use rpc::{ChainRpc, DynRpc, HttpRpc};
