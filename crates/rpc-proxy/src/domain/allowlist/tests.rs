use super::*;
use crate::domain::multicall;
use crate::domain::targets::Targets;
use alloy::primitives::address;
use serde_json::json;

const MASP: Address = address!("1111111111111111111111111111111111111111");
const PERMIT2: Address = address!("2222222222222222222222222222222222222222");
const TOKEN: Address = address!("3333333333333333333333333333333333333333");

/// `balanceOf(address)`, the call the whole deposit form depends on.
const BALANCE_OF: &str = "0x70a08231";

fn req(method: &str, params: serde_json::Value) -> Request {
    serde_json::from_value(json!({
        "jsonrpc": "2.0", "method": method, "params": params, "id": 1
    }))
    .expect("valid request")
}

fn check(p: &Policy, method: &str, params: serde_json::Value) -> Result<(), RpcError> {
    let m = Method::parse(method).expect("known method");
    p.validate(m, &req(method, params))
}

struct Fixture {
    targets: Targets,
}

impl Fixture {
    fn new() -> Self {
        Self {
            targets: Targets::new(MASP, PERMIT2, [TOKEN], []),
        }
    }
    fn policy(&self, tip: Option<u64>) -> Policy<'_> {
        Policy {
            masp: MASP,
            targets: &self.targets,
            max_log_range: 5_000,
            max_call_data_bytes: 8 * 1024,
            max_call_gas: 50_000_000,
            tip,
            deploy_block: None,
        }
    }

    /// A policy with a deployment floor, for the tests that exercise it.
    fn policy_from(&self, tip: Option<u64>, deploy_block: u64) -> Policy<'_> {
        Policy {
            deploy_block: Some(deploy_block),
            ..self.policy(tip)
        }
    }
}

#[test]
fn only_the_listed_methods_parse() {
    for m in [
        "eth_chainId",
        "eth_blockNumber",
        "eth_getBalance",
        "eth_call",
        "eth_getLogs",
        "eth_getBlockByNumber",
        "eth_getBlockByHash",
        "eth_getTransactionReceipt",
        "eth_getTransactionByHash",
    ] {
        assert!(Method::parse(m).is_some(), "{m} must be served");
    }

    // The write and fee methods, absent by decision rather than oversight.
    for m in [
        "eth_sendRawTransaction",
        "eth_estimateGas",
        "eth_getTransactionCount",
        "eth_feeHistory",
        "eth_maxPriorityFeePerGas",
        "eth_gasPrice",
        "eth_getProof",
        "eth_getStorageAt",
        "eth_subscribe",
        "net_version",
    ] {
        assert!(Method::parse(m).is_none(), "{m} must not be served");
    }
}

/// JSON-RPC method names are case-sensitive. Accepting a second spelling
/// would also mint a second cache key for every call made under it.
#[test]
fn method_matching_is_case_sensitive() {
    assert!(Method::parse("ETH_CALL").is_none());
    assert!(Method::parse("eth_Call").is_none());
}

/// The label set must stay closed and one-to-one, or a rejected method's
/// metric lands in another method's series.
#[test]
fn labels_are_the_wire_names_and_distinct() {
    let all = [
        Method::EthChainId,
        Method::EthBlockNumber,
        Method::EthGetBalance,
        Method::EthCall,
        Method::EthGetLogs,
        Method::EthGetBlockByNumber,
        Method::EthGetBlockByHash,
        Method::EthGetTransactionReceipt,
        Method::EthGetTransactionByHash,
    ];
    let labels: std::collections::HashSet<_> = all.iter().map(|m| m.label()).collect();
    assert_eq!(labels.len(), all.len());
    for m in all {
        assert_eq!(
            Method::parse(m.label()),
            Some(m),
            "{} round-trips",
            m.label()
        );
    }
}

/// A multicall is served as the calls inside it, so it is charged as them.
/// Charged as one call, a single request could spend a batch's worth of
/// upstream work for the price of one read.
#[test]
fn a_multicall_costs_every_call_it_carries() {
    use alloy::sol_types::SolCall;
    use chain_types::abi::IMulticall3;

    let calls = (0..5)
        .map(|_| IMulticall3::Call3 {
            target: Address::ZERO,
            allowFailure: true,
            callData: vec![0x70, 0xa0, 0x82, 0x31].into(),
        })
        .collect();
    let data = alloy::primitives::Bytes::from(IMulticall3::aggregate3Call { calls }.abi_encode());
    let params = [json!({"to": multicall::MULTICALL3.to_string(), "data": data.to_string()})];

    let one =
        Method::EthCall.weight(&[json!({"to": Address::ZERO.to_string(), "data": "0x70a08231"})]);
    assert_eq!(Method::EthCall.weight(&params), 5 * one);
}

/// A full block carries every transaction body; charging it as a header
/// would let the replacement-detection path run far past its budget.
#[test]
fn a_full_block_costs_more_than_a_header() {
    let m = Method::EthGetBlockByNumber;
    assert_eq!(m.weight(&[json!("latest"), json!(false)]), 2);
    assert_eq!(m.weight(&[json!("latest"), json!(true)]), 8);
    // An absent flag is `false` at every node.
    assert_eq!(m.weight(&[json!("latest")]), 2);
}

#[test]
fn a_no_argument_method_rejects_arguments() {
    let f = Fixture::new();
    let p = f.policy(Some(1_000));
    assert!(check(&p, "eth_chainId", json!([])).is_ok());
    assert!(check(&p, "eth_chainId", json!(["latest"])).is_err());
    assert!(check(&p, "eth_blockNumber", json!([1])).is_err());
}

/// Reading an object as an empty array would slip a filter past the range
/// check with no `fromBlock` to test.
#[test]
fn named_params_are_refused() {
    let f = Fixture::new();
    let p = f.policy(Some(1_000));
    let r = req("eth_getLogs", json!([]));
    let named = Request {
        params: Some(json!({"fromBlock": "0x1"})),
        ..r
    };
    assert!(p.validate(Method::EthGetLogs, &named).is_err());
}

#[test]
fn eth_call_accepts_the_sdks_own_shape() {
    let f = Fixture::new();
    let p = f.policy(Some(1_000));
    assert!(
        check(
            &p,
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": BALANCE_OF}, "latest"])
        )
        .is_ok()
    );
    // A trailing tag is optional; both spellings must be accepted or the
    // same logical call is served from one client and refused from another.
    assert!(
        check(
            &p,
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": BALANCE_OF}])
        )
        .is_ok()
    );
}

/// State overrides turn a bounded read into arbitrary compute against
/// modified state, on a metered node.
#[test]
fn eth_call_refuses_a_state_override() {
    let f = Fixture::new();
    let p = f.policy(Some(1_000));
    let err = check(
        &p,
        "eth_call",
        json!([
            {"to": TOKEN.to_string(), "data": BALANCE_OF},
            "latest",
            {TOKEN.to_string(): {"balance": "0xffff"}}
        ]),
    )
    .unwrap_err();
    assert!(err.message.contains("override"), "{}", err.message);
}

#[test]
fn eth_call_enforces_the_calldata_and_gas_caps() {
    let f = Fixture::new();
    let p = f.policy(Some(1_000));

    let big = format!("0x{}", "ab".repeat(9 * 1024));
    assert!(
        check(
            &p,
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": big}])
        )
        .is_err()
    );

    // 30M, a typical block gas limit.
    assert!(
        check(
            &p,
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": BALANCE_OF, "gas": "0x1c9c380"}])
        )
        .is_ok(),
        "30M gas is under the 50M cap"
    );
    // 100M, above the cap: either a mistake or an attempt to make one call
    // cost as much as the node will allow.
    assert!(
        check(
            &p,
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": BALANCE_OF, "gas": "0x5f5e100"}])
        )
        .is_err(),
        "100M gas is over it"
    );
}

/// The target allowlist is reached through `eth_call` validation, so a
/// stranger contract is refused before any upstream call is made.
#[test]
fn eth_call_refuses_an_unlisted_contract() {
    let f = Fixture::new();
    let p = f.policy(Some(1_000));
    let err = check(
        &p,
        "eth_call",
        json!([{"to": "0x9999999999999999999999999999999999999999", "data": BALANCE_OF}]),
    )
    .unwrap_err();
    assert!(err.message.contains("not served"), "{}", err.message);
}

/// What `fetchDepositEscrowed` actually sends: numeric `fromBlock`, literal
/// `"latest"` `toBlock`. If this fails, deposit cancellation is broken.
#[test]
fn eth_get_logs_accepts_the_sdks_own_shape() {
    let f = Fixture::new();
    let p = f.policy(Some(10_000));
    assert!(
        check(
            &p,
            "eth_getLogs",
            json!([{
                "address": MASP.to_string(),
                "fromBlock": "0x1900",   // 6400; 3600 below the tip
                "toBlock": "latest",
                "topics": ["0xaa"]
            }])
        )
        .is_ok()
    );
}

/// Without a required `address`, the proxy is a chain-wide log scraper
/// billed to us.
#[test]
fn eth_get_logs_requires_this_chains_pool() {
    let f = Fixture::new();
    let p = f.policy(Some(10_000));

    for filter in [
        json!({"fromBlock": "0x1900", "toBlock": "latest"}),
        json!({"address": TOKEN.to_string(), "fromBlock": "0x1900", "toBlock": "latest"}),
        // Two addresses is a wider query than the SDK ever makes.
        json!({"address": [MASP.to_string(), TOKEN.to_string()],
               "fromBlock": "0x1900", "toBlock": "latest"}),
    ] {
        assert!(
            check(&p, "eth_getLogs", json!([filter])).is_err(),
            "must require exactly this chain's pool"
        );
    }

    // The single-entry array form is legal and equivalent.
    assert!(
        check(
            &p,
            "eth_getLogs",
            json!([{"address": [MASP.to_string()], "fromBlock": "0x1900", "toBlock": "latest"}])
        )
        .is_ok()
    );
}

/// Each of these denotes a scan from genesis on a metered node.
#[test]
fn eth_get_logs_requires_an_explicit_from_block() {
    let f = Fixture::new();
    let p = f.policy(Some(10_000));

    for from in [None, Some(json!(null)), Some(json!("earliest"))] {
        let mut filter = serde_json::Map::new();
        filter.insert("address".into(), json!(MASP.to_string()));
        filter.insert("toBlock".into(), json!("latest"));
        if let Some(v) = from {
            filter.insert("fromBlock".into(), v);
        }
        assert!(
            check(&p, "eth_getLogs", json!([Value::Object(filter)])).is_err(),
            "an unbounded start must be refused"
        );
    }

    // Genesis named explicitly is still a full scan once the tip is far off.
    assert!(
        check(
            &p,
            "eth_getLogs",
            json!([{"address": MASP.to_string(), "fromBlock": "0x0", "toBlock": "latest"}])
        )
        .is_err(),
        "block zero to the tip exceeds the range cap"
    );
}

#[test]
fn eth_get_logs_enforces_the_range_cap() {
    let f = Fixture::new();
    let p = f.policy(Some(100_000));

    let ok = json!([{"address": MASP.to_string(), "fromBlock": "0x1", "toBlock": "0x1389"}]);
    assert!(
        check(&p, "eth_getLogs", ok).is_ok(),
        "5000 blocks is allowed"
    );

    let too_wide = json!([{"address": MASP.to_string(), "fromBlock": "0x1", "toBlock": "0x1771"}]);
    assert!(check(&p, "eth_getLogs", too_wide).is_err(), "6000 is not");
}

/// The cold-start case. `toBlock: "latest"` cannot be bounded without a
/// head, and guessing would mean an unbounded archive query. The service
/// resolves the head before validating, so this should not arise in
/// practice — but refusing is the safe reading when it does.
#[test]
fn eth_get_logs_refuses_an_open_range_when_the_head_is_unknown() {
    let f = Fixture::new();
    let err = check(
        &f.policy(None),
        "eth_getLogs",
        json!([{"address": MASP.to_string(), "fromBlock": "0x1", "toBlock": "latest"}]),
    )
    .unwrap_err();
    assert!(err.message.contains("head"), "{}", err.message);

    // A closed range needs no head and is unaffected.
    assert!(
        check(
            &f.policy(None),
            "eth_getLogs",
            json!([{"address": MASP.to_string(), "fromBlock": "0x1", "toBlock": "0x64"}])
        )
        .is_ok()
    );
}

#[test]
fn hash_shaped_methods_require_a_full_hash() {
    let f = Fixture::new();
    let p = f.policy(Some(1_000));
    let good = format!("0x{}", "ab".repeat(32));

    for m in [
        "eth_getTransactionReceipt",
        "eth_getTransactionByHash",
        "eth_getBlockByHash",
    ] {
        assert!(check(&p, m, json!([good])).is_ok(), "{m}");
        assert!(check(&p, m, json!(["0xabcd"])).is_err(), "{m} truncated");
        assert!(check(&p, m, json!([])).is_err(), "{m} missing");
    }
}

/// `earliest` is refused wherever a block tag is read, not only in
/// `eth_getLogs`.
#[test]
fn earliest_is_refused_on_every_method_that_takes_a_tag() {
    let f = Fixture::new();
    let p = f.policy(Some(1_000));
    let addr = format!("0x{}", "cd".repeat(20));

    assert!(check(&p, "eth_getBalance", json!([addr, "earliest"])).is_err());
    assert!(check(&p, "eth_getBlockByNumber", json!(["earliest", false])).is_err());
    assert!(
        check(
            &p,
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": BALANCE_OF}, "earliest"])
        )
        .is_err()
    );
}

/// Nothing this endpoint serves existed before the deployment, so a state
/// read below it asks a paid archive node about state that cannot exist.
#[test]
fn a_state_read_before_the_deployment_is_refused() {
    let f = Fixture::new();
    let p = f.policy_from(Some(1_000_000), 5_000);
    let addr = format!("0x{}", "cd".repeat(20));

    // 0x1000 == 4096, below the 5000 floor.
    let err = check(
        &p,
        "eth_call",
        json!([{"to": TOKEN.to_string(), "data": BALANCE_OF}, "0x1000"]),
    )
    .unwrap_err();
    assert!(
        err.message.contains("precedes this deployment"),
        "{}",
        err.message
    );

    assert!(check(&p, "eth_getBalance", json!([addr, "0x1000"])).is_err());

    // 0x1400 == 5120, above it.
    assert!(
        check(
            &p,
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": BALANCE_OF}, "0x1400"])
        )
        .is_ok()
    );
    // And the tip is always fine.
    assert!(
        check(
            &p,
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": BALANCE_OF}, "latest"])
        )
        .is_ok()
    );
}

/// A block header before the deployment is a coherent question, and needs
/// no archive state. Refusing it would add risk for no saving.
#[test]
fn the_floor_does_not_apply_to_block_headers() {
    let f = Fixture::new();
    assert!(
        check(
            &f.policy_from(Some(1_000_000), 5_000),
            "eth_getBlockByNumber",
            json!(["0x1000", false])
        )
        .is_ok()
    );
}

/// The load-bearing exception. `defaultFromBlock` floors at genesis whenever
/// the tip is under its 3600-block window — every dev chain and every fresh
/// e2e run — so flooring `fromBlock` would break deposit cancellation there.
/// The span cap is what bounds the cost instead.
#[test]
fn a_log_range_starting_before_the_deployment_is_still_served() {
    let f = Fixture::new();
    let p = f.policy_from(Some(4_000), 3_000);
    assert!(
        check(
            &p,
            "eth_getLogs",
            json!([{"address": MASP.to_string(), "fromBlock": "0x0", "toBlock": "latest"}])
        )
        .is_ok(),
        "genesis fromBlock is what a young chain actually sends"
    );
}

/// A range that ends before the pool existed can hold no log of it.
#[test]
fn a_log_range_entirely_before_the_deployment_is_refused() {
    let f = Fixture::new();
    let err = check(
        &f.policy_from(Some(1_000_000), 5_000),
        "eth_getLogs",
        json!([{"address": MASP.to_string(), "fromBlock": "0x1", "toBlock": "0x1000"}]),
    )
    .unwrap_err();
    assert!(
        err.message.contains("before this deployment"),
        "{}",
        err.message
    );
}

/// An unset floor is how a deployment that has not recorded its deploy
/// height behaves: unchanged.
#[test]
fn an_unset_floor_changes_nothing() {
    let f = Fixture::new();
    assert!(
        check(
            &f.policy(Some(1_000_000)),
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": BALANCE_OF}, "0x1"])
        )
        .is_ok()
    );
}

/// A historical read at an explicit height is the yield index's whole
/// access pattern, and it must stay allowed — it is what the archive
/// upstream is provisioned for.
#[test]
fn a_historical_call_at_an_explicit_height_is_allowed() {
    let f = Fixture::new();
    let p = f.policy(Some(1_000_000));
    assert!(
        check(
            &p,
            "eth_call",
            json!([{"to": MASP.to_string(), "data": "0x01e1d114"}, "0x1234"])
        )
        .is_err(),
        "totalAssets is not a MASP function"
    );
    assert!(
        check(
            &p,
            "eth_call",
            json!([{"to": TOKEN.to_string(), "data": BALANCE_OF}, "0x1234"])
        )
        .is_ok()
    );
}
