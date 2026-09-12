//! How long an answer may be reused, and whether it may be cached at all.
//!
//! Classification turns on finality. A read against a block deeper than
//! `reorg_depth` cannot change and is cacheable for as long as memory allows; a
//! read at the tip can change with the next block.
//!
//! When the chain head is unknown, every read classifies as if it were at the
//! tip. Cold start can therefore fail to cache an answer it could have, but
//! cannot serve a stale one.

use crate::domain::allowlist::Method;
use crate::domain::blocktag::BlockTag;
use crate::domain::jsonrpc::RpcError;
use serde_json::Value;
use std::time::Duration;

/// How long an entry lives, by what can invalidate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// The chain head. One second bounds the upstream to ~1 rps per chain
    /// regardless of how many wallets poll it.
    Head,
    /// Anything at the tip, or mined but still within reach of a reorg. Two
    /// seconds, close to viem's own 4s client-side `cacheTime`, so this cache
    /// is not the dominant source of staleness.
    Recent,
    /// Anything past `reorg_depth`: a read at or over such a height, or a
    /// block, receipt or transaction mined that deep. None of it can change.
    Finalized,
}

impl Class {
    pub fn ttl(self) -> Duration {
        match self {
            Class::Head => Duration::from_secs(1),
            Class::Recent => Duration::from_secs(2),
            Class::Finalized => Duration::from_secs(3600),
        }
    }

    /// Memory ceiling in bytes of cached result. A bound on memory, not a
    /// correctness property.
    ///
    /// Bytes rather than entries. Values here differ in size by orders of
    /// magnitude — an `eth_blockNumber` result is a dozen bytes and an
    /// `eth_getLogs` result can be hundreds of kilobytes — and the key space is
    /// caller-controlled, since a caller picks the block range and topics. An
    /// entry ceiling sized for the small case is no ceiling at all once the
    /// large ones arrive: 8192 log results at 200 KB is 1.6 GB from a bound
    /// that reads as generous.
    ///
    /// The totals are per chain, so a three-chain deployment is three times
    /// this.
    ///
    /// Weighted towards the long lifetime. A two-second class only ever holds
    /// what arrived in the last two seconds, so capacity beyond that buys
    /// nothing; an hour-long class keeps turning capacity into hits for as long
    /// as the entries stay.
    pub fn max_bytes(self) -> u64 {
        match self {
            // One entry per chain, effectively; sized for the entry rather than
            // for a population.
            Class::Head => 1 << 20,
            Class::Recent => 16 << 20,
            Class::Finalized => 64 << 20,
        }
    }

    /// Metric label. Closed set.
    pub fn label(self) -> &'static str {
        match self {
            Class::Head => "head",
            Class::Recent => "recent",
            Class::Finalized => "finalized",
        }
    }

    /// Every class, for wiring the caches and for exhaustive tests.
    pub const ALL: [Class; 3] = [Class::Head, Class::Recent, Class::Finalized];

    /// Every class [`classify_result`] can choose, most durable first. A
    /// result-classified entry may be in any of them, so a lookup probes them
    /// all.
    pub const FROM_RESULT: [Class; 2] = [Class::Finalized, Class::Recent];
}

/// How a request is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Answered from config; the upstream is never called.
    Local,
    /// Cached under this class, with concurrent callers collapsed onto one
    /// upstream call.
    Coalesce(Class),
    /// Forwarded, then classified from the *result* rather than the request.
    ///
    /// Needed where the request alone cannot say whether the answer is
    /// cacheable: a receipt query names a transaction hash, and whether that
    /// transaction is mined — and how deeply — is only visible in the response.
    /// Concurrent callers are still collapsed; only the class waits for the
    /// answer.
    ClassifyResult,
}

impl Policy {
    /// The class fixed by the request, if any. `None` both for a local answer
    /// and for one whose class is chosen from the result.
    pub fn class(self) -> Option<Class> {
        match self {
            Policy::Coalesce(class) => Some(class),
            Policy::Local | Policy::ClassifyResult => None,
        }
    }
}

/// How to serve `method` with these params.
pub fn policy(method: Method, params: &[Value], tip: Option<u64>, reorg_depth: u64) -> Policy {
    match method {
        Method::EthChainId => Policy::Local,

        Method::EthBlockNumber => Policy::Coalesce(Class::Head),

        // Addressed by hash, but a hash says nothing about whether the node has
        // the block yet. `null` from a load-balanced upstream that is a block
        // behind would otherwise be pinned; a found block is held by its depth.
        Method::EthGetBlockByHash => Policy::ClassifyResult,

        Method::EthGetBlockByNumber => {
            Policy::Coalesce(match BlockTag::parse(params.first()) {
                BlockTag::Number(n) if is_final(n, tip, reorg_depth) => Class::Finalized,
                // `latest` is the head under another name, and shares the head's
                // one-second bound rather than getting its own.
                BlockTag::Latest => Class::Head,
                _ => Class::Recent,
            })
        }

        // The block tag is the second parameter for both of these.
        Method::EthCall | Method::EthGetBalance => {
            Policy::Coalesce(match BlockTag::parse(params.get(1)) {
                BlockTag::Number(n) if is_final(n, tip, reorg_depth) => Class::Finalized,
                _ => Class::Recent,
            })
        }

        // A finalized range is immutable, so historical reads cost one upstream
        // call each and are then served from cache for an hour.
        Method::EthGetLogs => {
            let to = params
                .first()
                .and_then(Value::as_object)
                .and_then(|f| f.get("toBlock"))
                .and_then(|v| BlockTag::parse(Some(v)).number());
            Policy::Coalesce(match to {
                Some(n) if is_final(n, tip, reorg_depth) => Class::Finalized,
                _ => Class::Recent,
            })
        }

        Method::EthGetTransactionReceipt | Method::EthGetTransactionByHash => {
            Policy::ClassifyResult
        }
    }
}

/// The class for a result that has come back, given the height it reports.
///
/// `None` for a result that must not be cached at all: a `null` receipt means
/// "not mined yet", and caching that would add a full TTL of latency to every
/// deposit confirmation — the user watches a spinner for the cache to expire.
///
/// A mined-but-shallow result gets [`Class::Recent`] rather than
/// [`Class::Finalized`]: an hour is long enough for a reorg to have dropped it,
/// and serving a receipt for a transaction that no longer exists would resolve
/// a `waitForTransactionReceipt` on a transaction that never happened. Past
/// `reorg_depth` nothing can drop it, so it is held as long as any other
/// immutable answer.
pub fn classify_result(
    block_number: Option<u64>,
    tip: Option<u64>,
    reorg_depth: u64,
) -> Option<Class> {
    match block_number {
        None => None,
        Some(n) if is_final(n, tip, reorg_depth) => Some(Class::Finalized),
        Some(_) => Some(Class::Recent),
    }
}

/// Whether a JSON-RPC error answer may be cached like a result.
///
/// Only an `eth_call` revert. A revert is the EVM's deterministic answer at that
/// block — as much a fact about state as a returned value — so it is held
/// exactly as long as a result in the same class would be. Every other error
/// describes the node rather than the chain (`header not found`, a provider's
/// `-32005` limit, `missing trie node` from a pruning fallback) and must be
/// asked again.
///
/// A call that sets its own `gas` is excluded. `gas` is not part of the cache
/// key, and a revert caused by a subcall running out of that caller's allowance
/// would otherwise be served to callers who gave the call enough.
pub fn caches_error(method: Method, params: &[Value], error: &RpcError) -> bool {
    let sets_gas = params
        .first()
        .and_then(Value::as_object)
        .is_some_and(|call| call.contains_key("gas"));
    method == Method::EthCall && is_revert(error) && !sets_gas
}

/// Whether an error is the EVM reverting, rather than the node failing.
///
/// Code 3 is a revert carrying data. Geth reports a revert without data as
/// `-32000` with the message alone, so that shape is matched by its message.
pub fn is_revert(error: &RpcError) -> bool {
    error.code == 3 || (error.code == -32000 && error.message.starts_with("execution reverted"))
}

/// Whether `block` is deep enough that a reorg cannot reach it.
///
/// An unknown tip answers `false`. Every read then classifies as if it were at
/// the head, so the worst case is an entry that expires sooner than necessary.
fn is_final(block: u64, tip: Option<u64>, reorg_depth: u64) -> bool {
    tip.is_some_and(|t| t.saturating_sub(reorg_depth) >= block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DEPTH: u64 = 64;
    const TIP: u64 = 1_000_000;
    /// Deep enough to be unreachable by a reorg.
    const FINAL: &str = "0xf41c0"; // 999_360 == TIP - 640
    /// Mined, but within the reorg window.
    const SHALLOW: &str = "0xf423c"; // 999_996 == TIP - 4

    fn p(method: Method, params: serde_json::Value, tip: Option<u64>) -> Policy {
        let v: Vec<Value> = serde_json::from_value(params).unwrap();
        policy(method, &v, tip, DEPTH)
    }

    #[test]
    fn the_chain_id_is_never_forwarded() {
        assert_eq!(p(Method::EthChainId, json!([]), Some(TIP)), Policy::Local);
    }

    /// The head's one-second bound is what caps the upstream at ~1 rps per
    /// chain regardless of how many wallets poll it.
    #[test]
    fn the_head_is_capped_at_one_second() {
        assert_eq!(
            p(Method::EthBlockNumber, json!([]), Some(TIP)),
            Policy::Coalesce(Class::Head)
        );
        assert_eq!(Class::Head.ttl(), Duration::from_secs(1));
        // `latest` is the head under another name and shares its bound.
        assert_eq!(
            p(
                Method::EthGetBlockByNumber,
                json!(["latest", false]),
                Some(TIP)
            ),
            Policy::Coalesce(Class::Head)
        );
    }

    /// The finality split, on the two methods where it earns the most.
    #[test]
    fn a_read_at_a_finalized_height_is_cached_for_an_hour() {
        assert_eq!(
            p(Method::EthCall, json!([{"to": "0x1"}, FINAL]), Some(TIP)),
            Policy::Coalesce(Class::Finalized)
        );
        assert_eq!(
            p(
                Method::EthGetLogs,
                json!([{"address": "0x1", "fromBlock": "0x1", "toBlock": FINAL}]),
                Some(TIP)
            ),
            Policy::Coalesce(Class::Finalized)
        );
        assert_eq!(Class::Finalized.ttl(), Duration::from_secs(3600));
    }

    #[test]
    fn a_read_at_the_tip_is_cached_for_two_seconds() {
        for tag in [json!("latest"), json!(SHALLOW), Value::Null] {
            assert_eq!(
                p(Method::EthCall, json!([{"to": "0x1"}, tag]), Some(TIP)),
                Policy::Coalesce(Class::Recent)
            );
        }
        assert_eq!(
            p(
                Method::EthGetLogs,
                json!([{"address": "0x1", "fromBlock": "0x1", "toBlock": "latest"}]),
                Some(TIP)
            ),
            Policy::Coalesce(Class::Recent)
        );
    }

    /// The cold-start guarantee. With no head known, nothing may be treated as
    /// final — so the worst case is a short TTL, never a stale answer.
    #[test]
    fn an_unknown_head_never_classifies_anything_as_final() {
        for (m, params) in [
            (Method::EthCall, json!([{"to": "0x1"}, FINAL])),
            (Method::EthGetBalance, json!(["0x1", FINAL])),
            (Method::EthGetBlockByNumber, json!([FINAL, false])),
            (
                Method::EthGetLogs,
                json!([{"address": "0x1", "fromBlock": "0x1", "toBlock": FINAL}]),
            ),
        ] {
            match p(m, params, None) {
                Policy::Coalesce(c) => assert!(
                    c.ttl() <= Duration::from_secs(2),
                    "{} classified as {} with no known head",
                    m.label(),
                    c.label()
                ),
                other => panic!("{} became {other:?}", m.label()),
            }
        }
    }

    /// Receipt polling: the answer depends on the response, not the request.
    #[test]
    fn receipts_are_classified_from_their_result() {
        assert_eq!(
            p(Method::EthGetTransactionReceipt, json!(["0xab"]), Some(TIP)),
            Policy::ClassifyResult
        );
        assert_eq!(
            p(Method::EthGetTransactionByHash, json!(["0xab"]), Some(TIP)),
            Policy::ClassifyResult
        );
    }

    /// The deposit-confirmation case. Caching "not mined yet" would make every
    /// user watch the spinner for the TTL after their transaction lands.
    #[test]
    fn an_unmined_result_is_never_cached() {
        assert_eq!(classify_result(None, Some(TIP), DEPTH), None);
    }

    /// A shallow receipt can still be reorged away. Holding one for an hour
    /// could resolve a wait on a transaction that no longer exists.
    #[test]
    fn a_shallow_result_is_held_briefly_and_a_deep_one_for_an_hour() {
        assert_eq!(
            classify_result(Some(TIP - 4), Some(TIP), DEPTH),
            Some(Class::Recent)
        );
        assert_eq!(
            classify_result(Some(TIP - 640), Some(TIP), DEPTH),
            Some(Class::Finalized)
        );
    }

    /// The boundary itself, stated once so a later change to `is_final` cannot
    /// move it silently.
    #[test]
    fn the_finality_boundary_is_inclusive() {
        assert_eq!(
            classify_result(Some(TIP - DEPTH), Some(TIP), DEPTH),
            Some(Class::Finalized),
            "exactly reorg_depth deep is final"
        );
        assert_eq!(
            classify_result(Some(TIP - DEPTH + 1), Some(TIP), DEPTH),
            Some(Class::Recent),
            "one shallower is not"
        );
    }

    /// A result reporting a height above the head is a node that is ahead of
    /// the last observed head, not a final block.
    #[test]
    fn a_result_ahead_of_the_known_head_is_not_final() {
        assert_eq!(
            classify_result(Some(TIP + 10), Some(TIP), DEPTH),
            Some(Class::Recent)
        );
    }

    /// A hash does not say whether the node has the block yet, so a block by
    /// hash is classified like a receipt: `null` is never cached.
    #[test]
    fn a_block_by_hash_is_classified_from_its_result() {
        assert_eq!(
            p(Method::EthGetBlockByHash, json!(["0xab", false]), Some(TIP)),
            Policy::ClassifyResult
        );
    }

    /// `ChainService::cached` probes exactly [`Class::FROM_RESULT`] for a
    /// result-classified method. If this function ever returns a class outside
    /// it, values would be written to a cache nothing reads back — a silent
    /// zero-hit-rate rather than a failure. Pinned here, next to the function
    /// that would change.
    #[test]
    fn a_classified_result_only_ever_lands_in_a_probed_class() {
        let heights = [
            None,
            Some(0),
            Some(1),
            Some(TIP - DEPTH),
            Some(TIP),
            Some(TIP + 10),
        ];
        let tips = [None, Some(0), Some(TIP)];

        for h in heights {
            for t in tips {
                if let Some(c) = classify_result(h, t, DEPTH) {
                    assert!(
                        Class::FROM_RESULT.contains(&c),
                        "block {h:?} at tip {t:?} classified as {} — `cached` does not probe it",
                        c.label()
                    );
                }
            }
        }
    }

    fn err(code: i64, message: &str) -> RpcError {
        RpcError::new(code, message)
    }

    /// Both shapes a node reports a revert in are the chain's answer, and are
    /// held like one.
    #[test]
    fn an_eth_call_revert_is_cached() {
        let call = [json!({"to": "0x1", "data": "0x01"}), json!("latest")];
        assert!(caches_error(
            Method::EthCall,
            &call,
            &err(3, "execution reverted: paused")
        ));
        assert!(caches_error(
            Method::EthCall,
            &call,
            &err(-32000, "execution reverted")
        ));
    }

    /// An error about the node rather than the chain must be asked again, or a
    /// momentary provider hiccup is served as the answer for a whole TTL.
    #[test]
    fn a_node_error_is_never_cached() {
        let call = [json!({"to": "0x1", "data": "0x01"})];
        for e in [
            err(-32000, "header not found"),
            err(-32000, "missing trie node abc"),
            err(-32005, "limit exceeded"),
            err(-32603, "internal error"),
        ] {
            assert!(!caches_error(Method::EthCall, &call, &e), "{e:?}");
        }
    }

    /// Only `eth_call` reverts. A receipt query has no revert to report, and a
    /// code 3 on anything else is not a verdict this proxy understands.
    #[test]
    fn only_an_eth_call_error_is_cached() {
        assert!(!caches_error(
            Method::EthGetBalance,
            &[json!("0x1")],
            &err(3, "execution reverted")
        ));
    }

    /// `gas` is not in the key. A revert that one caller's gas cap produced
    /// must not be served to callers who did not set one.
    #[test]
    fn a_revert_under_a_caller_chosen_gas_cap_is_not_cached() {
        let capped = [json!({"to": "0x1", "data": "0x01", "gas": "0x5208"})];
        assert!(!caches_error(
            Method::EthCall,
            &capped,
            &err(3, "execution reverted")
        ));
    }

    /// Class labels are metric labels and must stay distinct.
    #[test]
    fn class_labels_are_distinct() {
        let labels: std::collections::HashSet<_> = Class::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(labels.len(), Class::ALL.len());
    }
}
