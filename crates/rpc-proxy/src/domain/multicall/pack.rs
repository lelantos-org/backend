//! Upstream: packing `eth_call` misses that share a block into one `aggregate3`.

use super::{MULTICALL3, as_result, result_bytes, target_and_data};
use crate::domain::allowlist::Method;
use crate::domain::blocktag::BlockTag;
use alloy::primitives::Bytes;
use alloy::sol_types::SolCall;
use chain_types::abi::IMulticall3;
use serde_json::value::RawValue;
use serde_json::{Value, json};

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
