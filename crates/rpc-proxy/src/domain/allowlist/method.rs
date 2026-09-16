//! The methods this endpoint serves, their wire names and their weights.

use crate::domain::multicall;
use serde_json::Value;

/// Compute units charged per call, modelled on Alchemy's meter so the rate
/// limiter's budget tracks the actual bill rather than a request count.
pub(super) mod weight {
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
