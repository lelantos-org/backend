//! Multicall3 `aggregate3`, in both directions.
//!
//! **Upstream**, several `eth_call` misses are packed into one. Clients send
//! ordinary calls, which are validated, keyed, cached and coalesced one by one;
//! only the round trip that fetches a set of misses packs them. A provider that
//! meters per `eth_call` then bills the set as one call.
//!
//! **From a client**, an `aggregate3` is unbundled. Forwarded whole, its
//! calldata is unique to each wallet's mix of reads, so it would never hit the
//! cache, and the calls inside it would be invisible to the allowlist and the
//! rate limit. Instead each inner call is served as the `eth_call` it is —
//! checked, charged, keyed, cached, coalesced, and packed again on the way up
//! with whatever else is missing — and the answers are encoded back into the
//! return `aggregate3` would have given. See [`requested`] and [`respond`].
//!
//! # What may be packed
//!
//! Inside `aggregate3` a call runs with Multicall3 as `msg.sender`, shares one
//! gas allowance with the rest of the pack, and carries no value. A call is
//! packed only when none of that can change its answer: no `from`, no `gas`, no
//! `value`. The allowlist is what makes the remainder safe — every function it
//! admits is a view over its arguments, and none reads `msg.sender`.
//!
//! One `aggregate3` executes against one block, so calls are packed per block
//! tag.
//!
//! # What is not trusted
//!
//! Only an inner call's *success* is taken from the pack. A failed inner call
//! could be a revert or could be the pack's shared gas running out, and the two
//! are indistinguishable from inside; so is a pack that comes back `0x` because
//! Multicall3 has no code at that block. Those are asked again unpacked, and the
//! node's own verdict is what the caller sees and what may be cached.

mod client;
mod pack;

pub use client::{Requested, call_failed, calls_in, requested, respond};
pub use pack::{Layout, Pack, layout, unpack};

use alloy::primitives::{Address, Bytes};
pub use chain_types::abi::MULTICALL3;
use serde_json::value::RawValue;
use serde_json::{Map, Value};

/// Return data as the `result` a node would have sent for the call on its own:
/// a lowercase `0x` hex string, `"0x"` when empty.
fn as_result(data: &Bytes) -> Box<RawValue> {
    RawValue::from_string(format!("\"{data}\"")).expect("a hex string is valid JSON")
}

/// The bytes of an `eth_call` result, which a node sends as a hex string.
pub fn result_bytes(raw: &RawValue) -> Option<Bytes> {
    serde_json::from_str::<&str>(raw.get()).ok()?.parse().ok()
}

/// An `eth_call` object's target, and its calldata under either field name.
/// `None` without a parseable target.
fn target_and_data(call: &Map<String, Value>) -> Option<(Address, Option<&str>)> {
    let target = call.get("to")?.as_str()?.parse().ok()?;
    let data = call
        .get("data")
        .or_else(|| call.get("input"))
        .and_then(Value::as_str);
    Some((target, data))
}

#[cfg(test)]
mod tests;
