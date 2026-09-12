//! Which JSON-RPC methods this endpoint serves, and what a valid call looks
//! like.
//!
//! Pure: no IO, no clock, no upstream. Everything the checks need arrives
//! through [`Policy`], so the whole surface is testable as a table.
//!
//! The list covers what the SDK issues and nothing further. Two entries
//! warrant explanation:
//!
//! - [`Method::EthGetTransactionByHash`] is **mandatory**, though nothing in the
//!   SDK calls it directly. viem's `waitForTransactionReceipt` calls
//!   `getTransaction` on every poll tick, and a `-32601` there escapes its
//!   `TransactionNotFoundError` recovery branch and rejects the whole wait — so
//!   omitting it breaks every deposit confirmation, browser ones included.
//! - [`Method::EthChainId`] is served from config and never forwarded. A chain
//!   id is immutable, so an upstream call for it would be redundant.
//!
//! Writes are absent by design: `eth_sendRawTransaction`, `eth_estimateGas`,
//! `eth_getTransactionCount`, `eth_feeHistory`. Browser writes go through the
//! user's own wallet over EIP-1193 and never arrive here. Accepting them would
//! make a public unauthenticated endpoint an open transaction relay billed to
//! our provider account, and `eth_estimateGas` in particular is unbounded
//! arbitrary-calldata compute — the cheapest way to burn a credit budget.

use crate::domain::blocktag::{BlockTag, is_address, is_hash32, parse_quantity};
use crate::domain::jsonrpc::{Request, RpcError};
use crate::domain::multicall;
use crate::domain::targets::{self, Targets};
use alloy::primitives::Address;
use serde_json::Value;

/// Compute units charged per call, modelled on Alchemy's meter so the rate
/// limiter's budget tracks the actual bill rather than a request count.
mod weight {
    pub const FREE: u32 = 0;
    pub const BLOCK_NUMBER: u32 = 1;
    pub const CHEAP_READ: u32 = 2;
    pub const CALL: u32 = 3;
    /// A block with full transaction bodies is substantially more work than a
    /// header. viem requests one on the replacement-detection path.
    pub const BLOCK_FULL: u32 = 8;
    /// The expensive one, and the reason the range checks below exist.
    pub const GET_LOGS: u32 = 15;
}

/// The methods this endpoint serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    EthChainId,
    EthBlockNumber,
    EthGetBalance,
    EthCall,
    EthGetLogs,
    EthGetBlockByNumber,
    EthGetBlockByHash,
    EthGetTransactionReceipt,
    EthGetTransactionByHash,
}

impl Method {
    /// Match a wire method name.
    ///
    /// Case-sensitive, as JSON-RPC method names are. `ETH_CALL` is a different
    /// method and is refused; accepting it would mint a second cache key for
    /// every call.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "eth_chainId" => Method::EthChainId,
            "eth_blockNumber" => Method::EthBlockNumber,
            "eth_getBalance" => Method::EthGetBalance,
            "eth_call" => Method::EthCall,
            "eth_getLogs" => Method::EthGetLogs,
            "eth_getBlockByNumber" => Method::EthGetBlockByNumber,
            "eth_getBlockByHash" => Method::EthGetBlockByHash,
            "eth_getTransactionReceipt" => Method::EthGetTransactionReceipt,
            "eth_getTransactionByHash" => Method::EthGetTransactionByHash,
            _ => return None,
        })
    }

    /// The wire name, and the metric label.
    ///
    /// `&'static str` from a closed set: the `method` label must never be the
    /// caller's raw string, or a scanner probing this endpoint mints an
    /// unbounded number of time series.
    pub fn label(self) -> &'static str {
        match self {
            Method::EthChainId => "eth_chainId",
            Method::EthBlockNumber => "eth_blockNumber",
            Method::EthGetBalance => "eth_getBalance",
            Method::EthCall => "eth_call",
            Method::EthGetLogs => "eth_getLogs",
            Method::EthGetBlockByNumber => "eth_getBlockByNumber",
            Method::EthGetBlockByHash => "eth_getBlockByHash",
            Method::EthGetTransactionReceipt => "eth_getTransactionReceipt",
            Method::EthGetTransactionByHash => "eth_getTransactionByHash",
        }
    }

    /// Compute units for this call, which depend on the params for the two
    /// block reads — a header and a full block are very different work — and
    /// for a multicall, which is charged for every call it carries.
    pub fn weight(self, params: &[Value]) -> u32 {
        match self {
            Method::EthChainId => weight::FREE,
            Method::EthBlockNumber => weight::BLOCK_NUMBER,
            Method::EthCall => {
                let calls = multicall::calls_in(params).unwrap_or(1).max(1);
                weight::CALL.saturating_mul(u32::try_from(calls).unwrap_or(u32::MAX))
            }
            Method::EthGetLogs => weight::GET_LOGS,
            Method::EthGetBlockByNumber | Method::EthGetBlockByHash => {
                if params.get(1).and_then(Value::as_bool).unwrap_or(false) {
                    weight::BLOCK_FULL
                } else {
                    weight::CHEAP_READ
                }
            }
            Method::EthGetBalance
            | Method::EthGetTransactionReceipt
            | Method::EthGetTransactionByHash => weight::CHEAP_READ,
        }
    }
}

/// The per-chain limits and allowlists validation reads.
pub struct Policy<'a> {
    /// This chain's pool. The only address `eth_getLogs` may query, because
    /// `fetchDepositEscrowed` is the SDK's only `getLogs` call site.
    pub masp: Address,
    pub targets: &'a Targets,
    pub max_log_range: u64,
    pub max_call_data_bytes: usize,
    pub max_call_gas: u64,
    /// Best known chain head, used to bound an `eth_getLogs` whose `toBlock` is
    /// `latest`. `None` means the head is not yet known; see
    /// [`Policy::validate`].
    pub tip: Option<u64>,
    /// Block the deployment's contracts were created in, if known.
    ///
    /// Nothing this endpoint serves existed before it: every `eth_call` target
    /// is one of our contracts, and the only `eth_getLogs` address is the pool.
    /// So a historical read below this floor asks about state that cannot
    /// exist — and asks a paid archive node, which is the expensive way to
    /// learn nothing.
    ///
    /// `None` disables the floor, which is what a deployment that has not
    /// recorded its deploy height gets.
    pub deploy_block: Option<u64>,
}

impl Policy<'_> {
    /// Whether `req` is a call this endpoint will make.
    ///
    /// Returns a JSON-RPC error rather than an HTTP one: the caller gets HTTP
    /// 200 with `-32601`/`-32602`, which viem surfaces as a legible error
    /// instead of retrying an opaque `HttpRequestError` three times first.
    pub fn validate(&self, method: Method, req: &Request) -> Result<(), RpcError> {
        if !req.params_are_positional() {
            return Err(RpcError::invalid_params(
                "params must be an array; named params are not supported",
            ));
        }
        let p = req.params();

        match method {
            Method::EthChainId | Method::EthBlockNumber => {
                if !p.is_empty() {
                    return Err(RpcError::invalid_params(format!(
                        "{} takes no parameters",
                        method.label()
                    )));
                }
            }

            Method::EthGetBalance => {
                let Some(addr) = p.first() else {
                    return Err(RpcError::invalid_params("eth_getBalance: missing address"));
                };
                if !is_address(addr) {
                    return Err(RpcError::invalid_params(
                        "eth_getBalance: malformed address",
                    ));
                }
                let tag = self.block_tag(p.get(1), "eth_getBalance")?;
                self.above_deployment(tag, "eth_getBalance")?;
            }

            Method::EthCall => self.validate_call(p)?,
            Method::EthGetLogs => self.validate_get_logs(p)?,

            Method::EthGetBlockByNumber => {
                self.block_tag(p.first(), "eth_getBlockByNumber")?;
            }

            Method::EthGetBlockByHash => {
                if !p.first().is_some_and(is_hash32) {
                    return Err(RpcError::invalid_params(
                        "eth_getBlockByHash: expected a 32-byte block hash",
                    ));
                }
            }

            Method::EthGetTransactionReceipt | Method::EthGetTransactionByHash => {
                if !p.first().is_some_and(is_hash32) {
                    return Err(RpcError::invalid_params(format!(
                        "{}: expected a 32-byte transaction hash",
                        method.label()
                    )));
                }
            }
        }
        Ok(())
    }

    /// A block tag that names a block this endpoint will read.
    ///
    /// `earliest` and anything unparseable are refused here, so no method has to
    /// remember to check.
    fn block_tag(&self, v: Option<&Value>, method: &str) -> Result<BlockTag, RpcError> {
        match BlockTag::parse(v) {
            BlockTag::Unknown => Err(RpcError::invalid_params(format!(
                "{method}: unsupported block tag (`earliest` and archive-wide reads are refused)"
            ))),
            tag => Ok(tag),
        }
    }

    /// Refuse a state read at a height before the contracts existed.
    ///
    /// Applied only to the two methods that read *state* at an explicit
    /// height — the ones a pruning node cannot answer and an archive node bills
    /// for. `latest` and the named tags are unaffected, and so is
    /// `eth_getBlockByNumber`: a block header before the deployment is a
    /// coherent question, just not one this app asks, and refusing it would add
    /// risk for no saving.
    fn above_deployment(&self, tag: BlockTag, method: &str) -> Result<(), RpcError> {
        let (Some(floor), Some(n)) = (self.deploy_block, tag.number()) else {
            return Ok(());
        };
        if n < floor {
            return Err(RpcError::invalid_params(format!(
                "{method}: block {n} precedes this deployment (first block {floor})"
            )));
        }
        Ok(())
    }

    fn validate_call(&self, p: &[Value]) -> Result<(), RpcError> {
        self.validate_call_envelope(p)?;
        let (to, selector) = targets::call_target(p);

        self.targets
            .check(to, selector)
            .map(|_| ())
            .map_err(|r| RpcError::invalid_params(r.message()))
    }

    /// Everything about an `eth_call` except the contract it reaches: its
    /// shape, block, gas cap and calldata size.
    ///
    /// Separate so a multicall can be held to it. Its own target is Multicall3,
    /// which no allowlist class names; the calls inside it are each validated
    /// in full as the `eth_call`s they are.
    pub fn validate_call_envelope(&self, p: &[Value]) -> Result<(), RpcError> {
        // The third parameter is a state or block override — a way to run
        // arbitrary code against modified state. Nothing the SDK does uses it,
        // and it turns a bounded read into unbounded compute on a paid node.
        if p.len() > 2 {
            return Err(RpcError::invalid_params(
                "eth_call: state and block overrides are not supported",
            ));
        }
        let Some(obj) = p.first().and_then(Value::as_object) else {
            return Err(RpcError::invalid_params("eth_call: missing call object"));
        };
        let tag = self.block_tag(p.get(1), "eth_call")?;
        self.above_deployment(tag, "eth_call")?;

        // A gas cap the caller sets above the block limit is either a mistake or
        // an attempt to make one call cost as much as possible.
        if let Some(g) = obj.get("gas") {
            let gas = g
                .as_str()
                .and_then(parse_quantity)
                .or_else(|| g.as_u64())
                .ok_or_else(|| RpcError::invalid_params("eth_call: malformed gas"))?;
            if gas > self.max_call_gas {
                return Err(RpcError::invalid_params(format!(
                    "eth_call: gas {gas} exceeds the {} allowed here",
                    self.max_call_gas
                )));
            }
        }

        let data_hex = obj
            .get("data")
            .or_else(|| obj.get("input"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if data_hex.len() / 2 > self.max_call_data_bytes {
            return Err(RpcError::invalid_params(format!(
                "eth_call: calldata exceeds {} bytes",
                self.max_call_data_bytes
            )));
        }
        Ok(())
    }

    /// The expensive method, and the one an abuser would reach for.
    ///
    /// Three independent bounds: only the pool's own logs, only a bounded range,
    /// and only a range that starts somewhere explicit.
    fn validate_get_logs(&self, p: &[Value]) -> Result<(), RpcError> {
        let Some(f) = p.first().and_then(Value::as_object) else {
            return Err(RpcError::invalid_params("eth_getLogs: missing filter"));
        };

        // `address` is required and must be the pool. Without it a caller
        // could scrape the whole chain through our upstream key. The SDK
        // queries only `MASP`, so the restriction excludes no real traffic.
        let addr = match f.get("address") {
            Some(Value::String(s)) => s.parse::<Address>().ok(),
            // The array form is legal JSON-RPC. Accepted only as a single-entry
            // array naming the pool, so it cannot be used to widen the query.
            Some(Value::Array(a)) if a.len() == 1 => {
                a[0].as_str().and_then(|s| s.parse::<Address>().ok())
            }
            _ => None,
        };
        if addr != Some(self.masp) {
            return Err(RpcError::invalid_params(
                "eth_getLogs: `address` must name this chain's pool",
            ));
        }

        // An absent or `null` `fromBlock` means genesis, which is an
        // archive-wide scan.
        let from = match f.get("fromBlock") {
            Some(v) => match BlockTag::parse(Some(v)).number() {
                Some(n) => n,
                None => {
                    return Err(RpcError::invalid_params(
                        "eth_getLogs: `fromBlock` must be an explicit block number",
                    ));
                }
            },
            None => {
                return Err(RpcError::invalid_params(
                    "eth_getLogs: `fromBlock` is required",
                ));
            }
        };

        // `toBlock: "latest"` is what the SDK actually sends, so it has to be
        // accepted — which means the range can only be bounded against a known
        // head. The service resolves the head before validating, so `None` here
        // is a cold-start state rather than a normal one; refusing is the safe
        // reading, since the alternative is an unbounded archive query.
        let to = match f.get("toBlock") {
            None | Some(Value::Null) => self.tip,
            Some(v) => match BlockTag::parse(Some(v)) {
                BlockTag::Number(n) => Some(n),
                BlockTag::Latest | BlockTag::Safe | BlockTag::Finalized => self.tip,
                BlockTag::Pending | BlockTag::Unknown => {
                    return Err(RpcError::invalid_params(
                        "eth_getLogs: unsupported `toBlock`",
                    ));
                }
            },
        };
        let Some(to) = to else {
            return Err(RpcError::invalid_params(
                "eth_getLogs: chain head not yet known; retry",
            ));
        };

        // A range that ends before the pool existed can hold no log of it. The
        // `fromBlock` end is deliberately NOT floored: `defaultFromBlock` in the
        // SDK returns genesis whenever the tip is under its 3600-block window,
        // which is every dev chain and every fresh e2e run, and rejecting that
        // would break deposit cancellation there. The span cap below already
        // bounds what a low `fromBlock` can cost.
        if let Some(floor) = self.deploy_block
            && to < floor
        {
            return Err(RpcError::invalid_params(format!(
                "eth_getLogs: range ends at {to}, before this deployment (first block {floor})"
            )));
        }

        // An inverted range is not an error at the node, it is an empty result —
        // but it is also a sign of a caller computing block numbers wrongly, and
        // saturating here keeps the span check well defined.
        let span = to.saturating_sub(from);
        if span > self.max_log_range {
            return Err(RpcError::invalid_params(format!(
                "eth_getLogs: range of {span} blocks exceeds the {} allowed here",
                self.max_log_range
            )));
        }

        // Four topic positions is the whole of the log-filter model; more is
        // malformed rather than merely unusual.
        if let Some(Value::Array(t)) = f.get("topics")
            && t.len() > 4
        {
            return Err(RpcError::invalid_params(
                "eth_getLogs: at most four topic positions",
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let data =
            alloy::primitives::Bytes::from(IMulticall3::aggregate3Call { calls }.abi_encode());
        let params = [json!({"to": multicall::MULTICALL3.to_string(), "data": data.to_string()})];

        let one = Method::EthCall
            .weight(&[json!({"to": Address::ZERO.to_string(), "data": "0x70a08231"})]);
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

        let too_wide =
            json!([{"address": MASP.to_string(), "fromBlock": "0x1", "toBlock": "0x1771"}]);
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
}
