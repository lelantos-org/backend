//! From a client: unbundling its `aggregate3` and answering it from the parts.

use super::{MULTICALL3, as_result, target_and_data};
use crate::domain::jsonrpc::RpcError;
use alloy::primitives::Bytes;
use alloy::sol_types::{Revert, SolCall, SolError};
use chain_types::abi::IMulticall3;
use serde_json::value::RawValue;
use serde_json::{Value, json};

/// One call inside a client's `aggregate3`, as the standalone `eth_call` it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Requested {
    /// `eth_call` params for this call alone, at the multicall's own block.
    pub params: Value,
    /// Whether `aggregate3` reports this call failing in place rather than
    /// reverting as a whole.
    pub allow_failure: bool,
}

/// The calls inside a client's `aggregate3`.
///
/// `None` when `params` is not an `eth_call` to Multicall3 at all. An error for
/// one that is but cannot be served: another Multicall3 function, or calldata
/// that does not decode.
///
/// The outer call's `from`, `gas` and `value` do not reach an inner call —
/// inside `aggregate3` each one runs with Multicall3 as its sender and no value
/// — so they are not carried over. The block is.
pub fn requested(params: &[Value]) -> Option<Result<Vec<Requested>, RpcError>> {
    let calls = decode_client_multicall(params)?;
    let tag = params.get(1);
    Some(calls.map(|calls| {
        calls
            .into_iter()
            .map(|c| {
                let mut params =
                    vec![json!({"to": c.target.to_string(), "data": c.callData.to_string()})];
                params.extend(tag.cloned());
                Requested {
                    params: Value::Array(params),
                    allow_failure: c.allowFailure,
                }
            })
            .collect()
    }))
}

/// How many calls a client's `aggregate3` carries, so it is charged for each.
/// `None` for anything that is not one that will be served.
pub fn calls_in(params: &[Value]) -> Option<usize> {
    decode_client_multicall(params)?
        .ok()
        .map(|calls| calls.len())
}

/// The `aggregate3` calls in an `eth_call` to Multicall3; see [`requested`].
fn decode_client_multicall(params: &[Value]) -> Option<Result<Vec<IMulticall3::Call3>, RpcError>> {
    let (target, data) = target_and_data(params.first()?.as_object()?)?;
    if target != MULTICALL3 {
        return None;
    }
    Some(decode_aggregate3(data.unwrap_or_default()))
}

fn decode_aggregate3(data: &str) -> Result<Vec<IMulticall3::Call3>, RpcError> {
    let bytes: Bytes = data
        .parse()
        .map_err(|_| RpcError::invalid_params("multicall: calldata is not hex"))?;
    // `aggregate3` is what viem sends. The older functions differ in how they
    // report a failure and whether they return a block, and nothing here
    // issues them.
    if !bytes.starts_with(&IMulticall3::aggregate3Call::SELECTOR) {
        return Err(RpcError::invalid_params(
            "multicall: only aggregate3 is served; send other calls as a JSON-RPC batch",
        ));
    }
    IMulticall3::aggregate3Call::abi_decode(&bytes, true)
        .map(|decoded| decoded.calls)
        .map_err(|_| RpcError::invalid_params("multicall: malformed aggregate3 calldata"))
}

/// The `aggregate3` return for these outcomes: whether each call succeeded,
/// with its return data or its revert data.
pub fn respond(outcomes: Vec<(bool, Bytes)>) -> Box<RawValue> {
    let results: Vec<IMulticall3::Result> = outcomes
        .into_iter()
        .map(|(success, return_data)| IMulticall3::Result {
            success,
            returnData: return_data,
        })
        .collect();
    as_result(&IMulticall3::aggregate3Call::abi_encode_returns(&(results,)).into())
}

/// What `aggregate3` reverts with when a call that was not allowed to fail
/// does, reported as a node reports any revert with a reason.
pub fn call_failed() -> RpcError {
    const REASON: &str = "Multicall3: call failed";
    let data = Revert {
        reason: REASON.into(),
    }
    .abi_encode();
    RpcError {
        code: 3,
        message: format!("execution reverted: {REASON}"),
        data: Some(Bytes::from(data).to_string()),
    }
}
