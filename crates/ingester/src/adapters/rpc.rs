use crate::domain::error::{IngesterError, RpcError};
use alloy::primitives::{Address, B256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::client::RpcClient;
use alloy::rpc::types::eth::{Filter, Log};
use alloy::transports::http::{Client, Http};
use async_trait::async_trait;
use chain_types::decode::known_signatures;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

#[async_trait]
pub trait ChainRpc: Send + Sync {
    async fn tip(&self) -> Result<u64, IngesterError>;
    async fn fetch_logs(
        &self,
        address: Address,
        from: u64,
        to: u64,
    ) -> Result<Vec<Log>, IngesterError>;
    async fn fetch_block_meta(
        &self,
        blocks: &[u64],
    ) -> Result<HashMap<u64, BlockMeta>, IngesterError>;
    /// Canonical hash at `n`, or `None` when the chain has no such block.
    ///
    /// The primitive reorg detection is built on: the stored cursor anchor is
    /// trustworthy only while the chain reports the same hash at the same height.
    async fn block_hash_at(&self, n: u64) -> Result<Option<B256>, IngesterError>;
}

/// Per-block facts the ingester needs beyond the log itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockMeta {
    pub timestamp: u64,
    /// What Solidity's `block.number` returns inside this block.
    ///
    /// Equal to the block's own height on Ethereum and OP-stack chains. On
    /// Arbitrum it is the L1 height, which is what MASP hashes into the deposit
    /// digest; replaying the L2 height there reverts `DigestMismatch`. Taken from
    /// the block's non-standard `l1BlockNumber` field when the node reports one.
    pub evm_block_number: u64,
}

pub type DynRpc = Arc<dyn ChainRpc>;

pub struct HttpRpc {
    inner: RootProvider<Http<Client>>,
    /// Cap on in-flight `eth_getBlockByNumber` calls in `fetch_block_meta`.
    meta_concurrency: usize,
}

impl HttpRpc {
    /// Build a provider with explicit timeouts.
    ///
    /// reqwest's default client has no request timeout, so a half-open socket
    /// would park the worker indefinitely while it holds its advisory lock,
    /// preventing any standby from taking over.
    pub fn build(
        rpc_url: &str,
        request_timeout: Duration,
        connect_timeout: Duration,
        meta_concurrency: usize,
    ) -> Result<Arc<Self>, IngesterError> {
        let url: Url = rpc_url
            .parse()
            .map_err(|e: url::ParseError| IngesterError::Config(format!("rpc_url: {}", e)))?;
        let http_client = Client::builder()
            .timeout(request_timeout)
            .connect_timeout(connect_timeout)
            .build()
            .map_err(|e| IngesterError::Config(format!("http client: {}", e)))?;
        let is_local = matches!(url.host_str(), Some("localhost") | Some("127.0.0.1"));
        let transport = Http::with_client(http_client, url);
        let inner = ProviderBuilder::new().on_client(RpcClient::new(transport, is_local));
        Ok(Arc::new(Self {
            inner,
            meta_concurrency: meta_concurrency.max(1),
        }))
    }
}

/// Substrings that mean "your query asked for too much".
///
/// Providers disagree on both the code and the wording. An unrecognised limit
/// error skips the halving path and fails the fetch instead of narrowing it.
/// Kept lowercase and matched against a lowercased message.
const RANGE_MARKERS: &[&str] = &[
    // Infura's limit-exceeded code.
    "-32005",
    "query returned more than",
    "block range is too large",
    "block range too large",
    "exceed maximum block range",
    "range is too large",
    "limit exceeded",
    "too many results",
    "response size exceeded",
    "response size",
    "log response size exceeded",
    "query timeout exceeded",
];

const RATE_LIMIT_MARKERS: &[&str] = &["429", "rate limit", "too many requests"];

/// `-32602` is a generic "invalid params". It is read as a range problem only
/// when the message also mentions the range or the result set; otherwise a
/// malformed filter would be halved indefinitely instead of surfacing.
fn is_oversized_params(msg: &str) -> bool {
    msg.contains("-32602") && ["range", "results", "logs"].iter().any(|w| msg.contains(w))
}

/// Map a provider error onto the taxonomy the callers act on.
fn classify<E: std::fmt::Display>(err: E) -> RpcError {
    let raw = err.to_string();
    let msg = raw.to_lowercase();

    if RANGE_MARKERS.iter().any(|m| msg.contains(m)) || is_oversized_params(&msg) {
        RpcError::RangeTooLarge
    } else if RATE_LIMIT_MARKERS.iter().any(|m| msg.contains(m)) {
        RpcError::RateLimited
    } else {
        RpcError::Other(raw)
    }
}

#[async_trait]
impl ChainRpc for HttpRpc {
    async fn tip(&self) -> Result<u64, IngesterError> {
        self.inner
            .get_block_number()
            .await
            .map_err(|e| IngesterError::from(classify(e)))
    }

    async fn fetch_logs(
        &self,
        address: Address,
        from: u64,
        to: u64,
    ) -> Result<Vec<Log>, IngesterError> {
        let sigs = known_signatures();
        let filter = Filter::new()
            .address(address)
            .event_signature(sigs.to_vec())
            .from_block(from)
            .to_block(to);
        self.inner
            .get_logs(&filter)
            .await
            .map_err(|e| IngesterError::from(classify(e)))
    }

    async fn fetch_block_meta(
        &self,
        block_numbers: &[u64],
    ) -> Result<HashMap<u64, BlockMeta>, IngesterError> {
        use futures::stream::{self, StreamExt, TryStreamExt};

        // Bounded fan-out. A large backfill chunk can touch thousands of distinct
        // blocks, and issuing one request per block at once invites rate limiting
        // and socket exhaustion.
        stream::iter(block_numbers.iter().copied().map(|n| {
            let p = self.inner.clone();
            async move {
                let blk = raw_block(&p, n).await?;
                let blk = blk.ok_or(IngesterError::Rpc(RpcError::BlockMissing(n)))?;
                let timestamp = hex_u64(&blk, "timestamp")
                    .ok_or(IngesterError::Rpc(RpcError::BlockMissing(n)))?;
                // Absent on every non-Arbitrum chain, where the block's own height
                // is what the EVM reports.
                let evm_block_number = hex_u64(&blk, "l1BlockNumber").unwrap_or(n);
                Ok::<(u64, BlockMeta), IngesterError>((
                    n,
                    BlockMeta {
                        timestamp,
                        evm_block_number,
                    },
                ))
            }
        }))
        .buffer_unordered(self.meta_concurrency)
        .try_collect()
        .await
    }

    async fn block_hash_at(&self, n: u64) -> Result<Option<B256>, IngesterError> {
        let Some(blk) = raw_block(&self.inner, n).await? else {
            return Ok(None);
        };
        let Some(h) = blk.get("hash").and_then(|v| v.as_str()) else {
            return Ok(None);
        };
        B256::from_str(h)
            .map(Some)
            .map_err(|e| IngesterError::Rpc(RpcError::Other(format!("block {} hash: {}", n, e))))
    }
}

/// Fetch a block header as raw JSON.
///
/// Raw request rather than the typed getter, because `l1BlockNumber` is an
/// Arbitrum extension that alloy's `Header` drops.
async fn raw_block(
    provider: &RootProvider<Http<Client>>,
    n: u64,
) -> Result<Option<serde_json::Value>, IngesterError> {
    provider
        .raw_request("eth_getBlockByNumber".into(), (format!("0x{n:x}"), false))
        .await
        .map_err(|e| IngesterError::from(classify(e)))
}

/// Read a `0x`-prefixed quantity from a JSON block object.
fn hex_u64(v: &serde_json::Value, key: &str) -> Option<u64> {
    let s = v.get(key)?.as_str()?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Error strings observed from the major providers. A miss on any of these
    /// means the adaptive fetch never halves and the backfill fails instead of
    /// narrowing its window.
    #[test]
    fn classifies_provider_range_errors() {
        let range = [
            "server returned an error response: error code -32005: query returned more than 10000 results",
            "error code -32602: Log response size exceeded. You can make eth_getLogs requests with up to a 2K block range",
            "query returned more than 10000 results",
            "block range is too large, max is 1000",
            "limit exceeded",
            "Query timeout exceeded. Consider reducing your block range",
        ];
        for e in range {
            assert!(
                matches!(classify(e), RpcError::RangeTooLarge),
                "expected RangeTooLarge for {e:?}, got {:?}",
                classify(e)
            );
        }
    }

    #[test]
    fn classifies_rate_limits() {
        for e in [
            "HTTP status 429 Too Many Requests",
            "your app has exceeded its rate limit",
        ] {
            assert!(
                matches!(classify(e), RpcError::RateLimited),
                "expected RateLimited for {e:?}"
            );
        }
    }

    /// A bare `-32602` is a generic "invalid params" and must not be read as a
    /// range problem, or the fetcher halves indefinitely over a malformed filter.
    #[test]
    fn leaves_unrelated_errors_alone() {
        for e in [
            "error code -32602: invalid argument 0: hex string has odd length",
            "connection reset by peer",
        ] {
            assert!(
                matches!(classify(e), RpcError::Other(_)),
                "expected Other for {e:?}, got {:?}",
                classify(e)
            );
        }
    }
}
