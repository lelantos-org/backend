use crate::domain::error::{IngesterError, RpcError};
use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::types::eth::{Filter, Log};
use alloy::transports::http::{Client, Http};
use async_trait::async_trait;
use chain_types::decode::known_signatures;
use std::collections::HashMap;
use std::sync::Arc;
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
}

/// Per-block facts the ingester needs beyond the log itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockMeta {
    pub timestamp: u64,
    /// What Solidity's `block.number` returns inside this block.
    ///
    /// Equal to the block's own height on Ethereum and OP-stack chains. On
    /// Arbitrum it is the *L1* height instead, which is what MASP hashes into
    /// the deposit digest — replaying the L2 height there reverts
    /// `DigestMismatch`. Taken from the block's non-standard `l1BlockNumber`
    /// field when the node reports one.
    pub evm_block_number: u64,
}

pub type DynRpc = Arc<dyn ChainRpc>;

pub struct HttpRpc {
    inner: RootProvider<Http<Client>>,
}

impl HttpRpc {
    pub fn build(rpc_url: &str) -> Result<Arc<Self>, IngesterError> {
        let url: Url = rpc_url
            .parse()
            .map_err(|e: url::ParseError| IngesterError::Config(format!("rpc_url: {}", e)))?;
        Ok(Arc::new(Self {
            inner: ProviderBuilder::new().on_http(url),
        }))
    }
}

fn classify<E: std::fmt::Display>(err: E) -> RpcError {
    let s = err.to_string();
    if s.contains("-32005") {
        RpcError::RangeTooLarge
    } else if s.contains("response size") {
        RpcError::ResponseTooLarge
    } else if s.contains("429") || s.to_lowercase().contains("rate limit") {
        RpcError::RateLimited
    } else {
        RpcError::Other(s)
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
        use futures::stream::{FuturesUnordered, StreamExt};
        let mut futs = FuturesUnordered::new();
        for &n in block_numbers {
            let p = self.inner.clone();
            futs.push(async move {
                // Raw request rather than the typed getter: `l1BlockNumber` is
                // an Arbitrum extension that alloy's `Header` drops.
                let blk: Option<serde_json::Value> = p
                    .raw_request("eth_getBlockByNumber".into(), (format!("0x{n:x}"), false))
                    .await
                    .map_err(|e| IngesterError::from(classify(e)))?;
                let blk = blk.ok_or(IngesterError::Rpc(RpcError::BlockMissing(n)))?;

                let timestamp = hex_u64(&blk, "timestamp")
                    .ok_or(IngesterError::Rpc(RpcError::BlockMissing(n)))?;
                // Absent on every non-Arbitrum chain, where the block's own
                // height is what the EVM reports.
                let evm_block_number = hex_u64(&blk, "l1BlockNumber").unwrap_or(n);

                Ok::<(u64, BlockMeta), IngesterError>((
                    n,
                    BlockMeta {
                        timestamp,
                        evm_block_number,
                    },
                ))
            });
        }
        let mut out = HashMap::new();
        while let Some(r) = futs.next().await {
            let (n, meta) = r?;
            out.insert(n, meta);
        }
        Ok(out)
    }
}

/// Read a `0x`-prefixed quantity from a JSON block object.
fn hex_u64(v: &serde_json::Value, key: &str) -> Option<u64> {
    let s = v.get(key)?.as_str()?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}
