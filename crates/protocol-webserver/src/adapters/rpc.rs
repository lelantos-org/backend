//! The chain transport, from `chain_types::rpc`.
//!
//! The retry/backoff boilerplate this file used to hold was duplicated verbatim
//! between the relayer and protocol-webserver; it now lives in `chain-types`
//! behind its `rpc` feature. What stays here is the one thing that differs per
//! service: the deadline.

pub use chain_types::rpc::{HttpTransport, RpcEndpoint};

use chain_types::rpc::RpcTimeouts;

/// Deadline for a single JSON-RPC call.
///
/// Generous compared to the relayer's, because every call made here is a
/// historical read against archive state, which is markedly slower than a call
/// at the head. Still bounded: an untimed call against a hung node would stall a
/// chain's measurement forever while holding its election lock.
const TIMEOUTS: RpcTimeouts = RpcTimeouts::request(30);

/// This service's endpoint for `rpc_url`.
///
/// The error is a string, as `chain_types::rpc` returns it; every caller here
/// already frames it with its own context.
pub fn endpoint(rpc_url: &str) -> Result<RpcEndpoint, String> {
    RpcEndpoint::new(rpc_url, TIMEOUTS)
}
