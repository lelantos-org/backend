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

mod method;

pub use method::Method;

use crate::domain::blocktag::{BlockTag, is_address, is_hash32, parse_quantity};
use crate::domain::jsonrpc::{Request, RpcError};
use crate::domain::targets::{self, Targets};
use alloy::primitives::Address;
use serde_json::Value;

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
mod tests;
