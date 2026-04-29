use crate::domain::error::{IngesterError, RpcError};
use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::types::eth::{BlockNumberOrTag, Filter, Log};
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
    async fn fetch_block_timestamps(
        &self,
        blocks: &[u64],
    ) -> Result<HashMap<u64, u64>, IngesterError>;
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

    async fn fetch_block_timestamps(
        &self,
        block_numbers: &[u64],
    ) -> Result<HashMap<u64, u64>, IngesterError> {
        use futures::stream::{FuturesUnordered, StreamExt};
        let mut futs = FuturesUnordered::new();
        for &n in block_numbers {
            let p = self.inner.clone();
            futs.push(async move {
                let blk = p
                    .get_block_by_number(BlockNumberOrTag::Number(n), false)
                    .await
                    .map_err(|e| IngesterError::from(classify(e)))?;
                let ts = blk
                    .ok_or(IngesterError::Rpc(RpcError::BlockMissing(n)))?
                    .header
                    .timestamp;
                Ok::<(u64, u64), IngesterError>((n, ts))
            });
        }
        let mut out = HashMap::new();
        while let Some(r) = futs.next().await {
            let (n, ts) = r?;
            out.insert(n, ts);
        }
        Ok(out)
    }
}
