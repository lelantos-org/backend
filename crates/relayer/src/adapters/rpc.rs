//! The chain transport, from `chain_types::rpc`.
//!
//! The retry/backoff boilerplate this file used to hold was duplicated verbatim
//! between the relayer and registry-webserver; it now lives in `chain-types`
//! behind its `rpc` feature. What stays here is the one thing that differs per
//! service: the deadline.

pub use chain_types::rpc::{HttpTransport, RpcEndpoint};

use chain_types::rpc::RpcTimeouts;

/// Deadline for a single JSON-RPC call.
///
/// Every call on the submission path is one fast round trip, so this sits well
/// above a slow `eth_estimateGas` and well below the OS-level TCP timeout that
/// would otherwise govern. It is required because the per-chain tree-mirror mutex
/// is held across submission: an untimed call against a hung node would hold that
/// chain's mutex, and every spend, swap and flush queued behind it, indefinitely.
///
/// Unrelated to `receipt_timeout_s`, which bounds a loop of short polls rather
/// than any single call.
const TIMEOUTS: RpcTimeouts = RpcTimeouts::request(15);

/// This service's endpoint for `rpc_url`.
///
/// The error is a string, as `chain_types::rpc` returns it; every caller here
/// already frames it with its own context.
pub fn endpoint(rpc_url: &str) -> Result<RpcEndpoint, String> {
    RpcEndpoint::new(rpc_url, TIMEOUTS)
}
