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

use crate::domain::allowlist::Method;
use crate::domain::blocktag::BlockTag;
use crate::domain::jsonrpc::RpcError;
use alloy::primitives::{Address, Bytes};
use alloy::sol_types::{Revert, SolCall, SolError};
use chain_types::abi::IMulticall3;
pub use chain_types::abi::MULTICALL3;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};

/// Fewest calls worth packing. A pack of one saves nothing and costs the
/// encoding.
const MIN_PACK: usize = 2;

/// Several calls, sent upstream as one.
#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
    /// Positions of the packed calls in the caller's list, in pack order.
    pub members: Vec<usize>,
    /// `eth_call` params addressing Multicall3.
    pub params: Value,
}

/// How a set of calls goes upstream: some on their own, the rest in packs.
/// Every position appears exactly once, in `alone` or in one pack.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    /// Positions of the calls sent as they are, ascending.
    pub alone: Vec<usize>,
    pub packs: Vec<Pack>,
}

impl Layout {
    /// All `n` calls on their own.
    pub fn unpacked(n: usize) -> Self {
        Self {
            alone: (0..n).collect(),
            packs: Vec::new(),
        }
    }
}

/// Lay `calls` out for the trip upstream, packing what may be packed by block
/// tag.
///
/// A call that cannot be packed, or has no other call at its block, goes alone.
pub fn layout<'a>(calls: impl IntoIterator<Item = (Method, &'a [Value])>) -> Layout {
    let mut layout = Layout::default();
    let mut groups: Vec<(BlockTag, Vec<usize>, Vec<IMulticall3::Call3>)> = Vec::new();
    for (i, (method, params)) in calls.into_iter().enumerate() {
        let Some((tag, call)) = packable(method, params) else {
            layout.alone.push(i);
            continue;
        };
        match groups.iter_mut().find(|(t, ..)| *t == tag) {
            Some((_, members, pack)) => {
                members.push(i);
                pack.push(call);
            }
            None => groups.push((tag, vec![i], vec![call])),
        }
    }

    for (tag, members, calls) in groups {
        if members.len() < MIN_PACK {
            layout.alone.extend(members);
            continue;
        }
        let data = Bytes::from(IMulticall3::aggregate3Call { calls }.abi_encode());
        layout.packs.push(Pack {
            members,
            params: json!([
                {"to": MULTICALL3.to_string(), "data": data.to_string()},
                tag.canonical(),
            ]),
        });
    }
    layout.alone.sort_unstable();
    layout
}

/// The block and inner call for an `eth_call` that may be packed.
fn packable(method: Method, params: &[Value]) -> Option<(BlockTag, IMulticall3::Call3)> {
    if method != Method::EthCall {
        return None;
    }
    let call = params.first()?.as_object()?;
    if ["from", "gas", "value"]
        .iter()
        .any(|k| call.contains_key(*k))
    {
        return None;
    }
    let (target, data) = target_and_data(call)?;
    let call_data: Bytes = data?.parse().ok()?;
    let tag = match BlockTag::parse(params.get(1)) {
        BlockTag::Unknown => return None,
        tag => tag,
    };
    Some((
        tag,
        IMulticall3::Call3 {
            target,
            // Failures are re-asked unpacked, so one revert must not take the
            // pack's successes down with it.
            allowFailure: true,
            callData: call_data,
        },
    ))
}

/// Each inner call's result from a pack's answer, or `None` for one that
/// failed.
///
/// `None` overall for an answer that is not an `aggregate3` return for `n`
/// calls — which is what `0x` from an address without code is.
pub fn unpack(raw: &RawValue, n: usize) -> Option<Vec<Option<Box<RawValue>>>> {
    let bytes = result_bytes(raw)?;
    let results = IMulticall3::aggregate3Call::abi_decode_returns(&bytes, true)
        .ok()?
        .returnData;
    if results.len() != n {
        return None;
    }
    Some(
        results
            .into_iter()
            .map(|r| r.success.then(|| as_result(&r.returnData)))
            .collect(),
    )
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0x3333333333333333333333333333333333333333";
    const OTHER: &str = "0x4444444444444444444444444444444444444444";

    fn call(fields: Value, tag: Value) -> Vec<Value> {
        vec![fields, tag]
    }

    fn read(to: &str, data: &str) -> Vec<Value> {
        call(json!({"to": to, "data": data}), json!("latest"))
    }

    fn laid_out(calls: &[Vec<Value>]) -> Layout {
        layout(calls.iter().map(|p| (Method::EthCall, p.as_slice())))
    }

    fn members(calls: &[Vec<Value>]) -> Vec<Vec<usize>> {
        laid_out(calls)
            .packs
            .into_iter()
            .map(|p| p.members)
            .collect()
    }

    /// An answer as Multicall3 would encode it.
    fn answer(results: Vec<(bool, &str)>) -> Box<RawValue> {
        let results: Vec<IMulticall3::Result> = results
            .into_iter()
            .map(|(success, data)| IMulticall3::Result {
                success,
                returnData: data.parse().unwrap(),
            })
            .collect();
        let bytes = Bytes::from(IMulticall3::aggregate3Call::abi_encode_returns(&(results,)));
        RawValue::from_string(format!("\"{bytes}\"")).unwrap()
    }

    #[test]
    fn calls_at_one_block_share_a_pack() {
        let calls = [
            read(TOKEN, "0x01"),
            read(OTHER, "0x02"),
            read(TOKEN, "0x03"),
        ];
        assert_eq!(members(&calls), vec![vec![0, 1, 2]]);
    }

    /// One `aggregate3` executes against one block. Mixing tags would answer
    /// a historical read at the tip.
    #[test]
    fn calls_at_different_blocks_never_share_a_pack() {
        let at = |tag: Value| call(json!({"to": TOKEN, "data": "0x01"}), tag);
        let calls = [
            at(json!("latest")),
            at(json!("0x10")),
            at(json!("0x10")),
            at(Value::Null),
        ];
        // An omitted tag is `latest`, so it joins the first.
        assert_eq!(members(&calls), vec![vec![0, 3], vec![1, 2]]);
    }

    /// Inside a pack `msg.sender` is Multicall3, gas is shared and no value is
    /// sent. A call that names any of those would be answered a different
    /// question.
    #[test]
    fn a_call_that_depends_on_its_context_is_never_packed() {
        for field in ["from", "gas", "value"] {
            let mut fields = json!({"to": TOKEN, "data": "0x01"});
            fields[field] = json!("0x1");
            let calls = [
                call(fields, json!("latest")),
                read(TOKEN, "0x02"),
                read(OTHER, "0x03"),
            ];
            let laid = laid_out(&calls);
            assert_eq!(laid.alone, vec![0], "{field}");
            assert_eq!(laid.packs[0].members, vec![1, 2], "{field}");
        }
    }

    #[test]
    fn a_lone_packable_call_goes_alone() {
        let calls = [
            read(TOKEN, "0x01"),
            call(
                json!({"to": TOKEN, "from": OTHER, "data": "0x02"}),
                json!("latest"),
            ),
        ];
        assert_eq!(laid_out(&calls), Layout::unpacked(2));
    }

    /// Every call is placed once: alone, or in exactly one pack.
    #[test]
    fn a_layout_places_every_call_once() {
        let calls = [
            read(TOKEN, "0x01"),
            call(json!({"to": TOKEN, "data": "0x02"}), json!("0x10")),
            read(OTHER, "0x03"),
            read(TOKEN, "0xnothex"),
        ];
        let laid = laid_out(&calls);
        let mut placed: Vec<usize> = laid
            .packs
            .iter()
            .flat_map(|p| p.members.iter().copied())
            .chain(laid.alone.iter().copied())
            .collect();
        placed.sort_unstable();
        assert_eq!(placed, vec![0, 1, 2, 3]);
        assert_eq!(laid.alone, vec![1, 3]);
    }

    #[test]
    fn only_eth_call_is_packed() {
        let p = read(TOKEN, "0x01");
        let calls = [
            (Method::EthGetBalance, p.as_slice()),
            (Method::EthGetBalance, p.as_slice()),
        ];
        assert_eq!(layout(calls), Layout::unpacked(2));
    }

    /// Malformed calldata is the node's to refuse, one call at a time.
    #[test]
    fn unparseable_calldata_is_not_packed() {
        let calls = [
            read(TOKEN, "0xnothex"),
            read(TOKEN, "0x01"),
            read(OTHER, "0x02"),
        ];
        assert_eq!(members(&calls), vec![vec![1, 2]]);
    }

    /// The pack addresses Multicall3 at the members' own block, and carries
    /// their calls in member order.
    #[test]
    fn a_pack_encodes_its_members_in_order() {
        let calls = [read(TOKEN, "0x70a08231"), read(OTHER, "0x313ce567")];
        let [pack] = laid_out(&calls).packs.try_into().unwrap();
        let params = pack.params.as_array().unwrap();
        assert_eq!(params[1], "latest");
        assert_eq!(params[0]["to"], MULTICALL3.to_string());

        let data: Bytes = params[0]["data"].as_str().unwrap().parse().unwrap();
        let decoded = IMulticall3::aggregate3Call::abi_decode(&data, true).unwrap();
        let inner: Vec<(String, String)> = decoded
            .calls
            .iter()
            .map(|c| (c.target.to_string().to_lowercase(), c.callData.to_string()))
            .collect();
        assert_eq!(
            inner,
            vec![
                (TOKEN.to_string(), "0x70a08231".to_string()),
                (OTHER.to_string(), "0x313ce567".to_string()),
            ]
        );
        assert!(decoded.calls.iter().all(|c| c.allowFailure));
    }

    /// A success reads exactly as the node's own answer would; a failure is
    /// left for the caller to re-ask.
    #[test]
    fn an_answer_unpacks_into_results_and_failures() {
        let results = unpack(
            &answer(vec![(true, "0x2a"), (false, "0x08c379a0"), (true, "0x")]),
            3,
        )
        .unwrap();
        let text: Vec<Option<String>> = results
            .iter()
            .map(|r| r.as_ref().map(|r| r.get().to_string()))
            .collect();
        assert_eq!(
            text,
            vec![Some("\"0x2a\"".into()), None, Some("\"0x\"".into())]
        );
    }

    /// No code at Multicall3 answers `0x`, which is not a pack's answer at all.
    #[test]
    fn an_answer_from_an_address_without_code_does_not_unpack() {
        let empty = RawValue::from_string("\"0x\"".into()).unwrap();
        assert!(unpack(&empty, 2).is_none());
    }

    /// A result per call, or the answer is not for this pack.
    #[test]
    fn an_answer_of_the_wrong_length_does_not_unpack() {
        assert!(unpack(&answer(vec![(true, "0x01")]), 2).is_none());
    }

    /// A client's `aggregate3`, addressed to Multicall3 with `extra` fields.
    fn client_multicall(
        calls: &[(&str, &str, bool)],
        extra: Value,
        tag: Option<Value>,
    ) -> Vec<Value> {
        let calls = calls
            .iter()
            .map(|(target, data, allow)| IMulticall3::Call3 {
                target: target.parse().unwrap(),
                allowFailure: *allow,
                callData: data.parse().unwrap(),
            })
            .collect();
        let data = Bytes::from(IMulticall3::aggregate3Call { calls }.abi_encode());
        let mut call = json!({"to": MULTICALL3.to_string(), "data": data.to_string()});
        call.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mut params = vec![call];
        params.extend(tag);
        params
    }

    #[test]
    fn a_call_to_anything_but_multicall3_is_not_a_multicall() {
        assert_eq!(requested(&read(TOKEN, "0x70a08231")), None);
        assert_eq!(requested(&[]), None);
    }

    /// Each inner call becomes the `eth_call` it would be on its own, at the
    /// multicall's block — and, like any `eth_call`, with no `from`, `gas` or
    /// `value`, none of which reaches it inside `aggregate3`.
    #[test]
    fn an_aggregate3_unbundles_into_standalone_calls_at_its_block() {
        let params = client_multicall(
            &[(TOKEN, "0x70a08231", false), (OTHER, "0x313ce567", true)],
            json!({"from": OTHER, "gas": "0x5208"}),
            Some(json!("0x10")),
        );
        let calls = requested(&params).unwrap().unwrap();
        let shown: Vec<(String, String, Value, bool)> = calls
            .iter()
            .map(|c| {
                let p = c.params.as_array().unwrap();
                assert!(p[0].get("from").is_none() && p[0].get("gas").is_none());
                (
                    p[0]["to"].as_str().unwrap().to_lowercase(),
                    p[0]["data"].as_str().unwrap().to_string(),
                    p[1].clone(),
                    c.allow_failure,
                )
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                (TOKEN.into(), "0x70a08231".into(), json!("0x10"), false),
                (OTHER.into(), "0x313ce567".into(), json!("0x10"), true),
            ]
        );
    }

    /// No tag on the multicall is no tag on its calls, which keys them exactly
    /// as a plain read at `latest`.
    #[test]
    fn an_untagged_aggregate3_leaves_its_calls_untagged() {
        let params = client_multicall(&[(TOKEN, "0x01", true)], json!({}), None);
        let calls = requested(&params).unwrap().unwrap();
        assert_eq!(calls[0].params.as_array().unwrap().len(), 1);
    }

    #[test]
    fn another_multicall3_function_is_refused() {
        let try_aggregate = [json!({"to": MULTICALL3.to_string(), "data": "0xbce38bd7"})];
        let err = requested(&try_aggregate).unwrap().unwrap_err();
        assert!(err.message.contains("only aggregate3"), "{err:?}");
    }

    #[test]
    fn malformed_aggregate3_calldata_is_refused() {
        let truncated = [json!({"to": MULTICALL3.to_string(), "data": "0x82ad56cb0000"})];
        assert!(requested(&truncated).unwrap().is_err());
    }

    /// The return a client decodes must be exactly what Multicall3 would have
    /// sent for the same outcomes.
    #[test]
    fn a_response_encodes_as_aggregate3_would() {
        let raw = respond(vec![
            (true, "0x2a".parse().unwrap()),
            (false, "0x1234".parse().unwrap()),
        ]);
        assert_eq!(
            raw.get(),
            answer(vec![(true, "0x2a"), (false, "0x1234")]).get()
        );
    }

    /// A reason-carrying revert, decodable as `Error(string)`.
    #[test]
    fn a_disallowed_failure_reverts_as_multicall3_does() {
        let e = call_failed();
        assert_eq!(e.code, 3);
        let data: Bytes = e.data.unwrap().parse().unwrap();
        assert_eq!(
            Revert::abi_decode(&data, true).unwrap().reason,
            "Multicall3: call failed"
        );
    }
}
