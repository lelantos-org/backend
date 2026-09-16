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
