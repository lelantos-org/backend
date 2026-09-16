//! HTTP JSON-RPC adapter for one chain.
//!
//! Owns the domain-shaped [`ChainRpc`] trait and the only implementation of
//! it. This crate is `ARCHITECTURE.md`'s one deliberate exception to building
//! on `chain_types::rpc::RpcEndpoint`: the trait is the seam the reorg and
//! windowing tests substitute, and both timeouts come from this binary's own
//! config.

mod classify;

use crate::domain::error::{IngesterError, RpcError};
use crate::domain::models::BlockMeta;
use alloy::primitives::{Address, B256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::client::RpcClient;
use alloy::rpc::types::eth::{Filter, Log};
use alloy::transports::http::{Client, Http};
use async_trait::async_trait;
use chain_types::decode::known_signatures;
use classify::classify;
use shared::metrics::{record_rpc_call, record_rpc_error};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
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

pub type DynRpc = Arc<dyn ChainRpc>;

/// What [`HttpRpc`] needs to reach one chain.
///
/// Declared here rather than taking the binary's `ChainConfig`: an adapter sits
/// below `app/`, so pointing it at the TOML schema would make a serde rename a
/// compile break in the RPC client and would leave a second provider no way to
/// be built from anything else. `app::config` owns the conversion.
#[derive(Debug, Clone)]
pub struct RpcConfig {
    pub url: String,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    /// Cap on in-flight `eth_getBlockByNumber` calls.
    pub meta_concurrency: usize,
    /// Labels this provider's call and error counters.
    pub chain_id: i64,
}

pub struct HttpRpc {
    inner: RootProvider<Http<Client>>,
    /// Cap on in-flight `eth_getBlockByNumber` calls, held per provider rather
    /// than per call.
    ///
    /// A per-call `buffer_unordered` bounds one `fetch_block_meta`, not the
    /// chain: the backfill runs `backfill_concurrency` chunks at once, each
    /// entering `fetch_block_meta`, so a per-call cap is silently multiplied by
    /// the chunk concurrency. A semaphore on the provider is the cap the config
    /// actually promises.
    meta_permits: Arc<Semaphore>,
    /// Labels this provider's call and error counters. Carried here rather than
    /// threaded through every method: one `HttpRpc` serves exactly one chain.
    chain_id: i64,
}

impl HttpRpc {
    /// Build a provider for one chain, with explicit timeouts.
    ///
    /// reqwest's default client has no request timeout, so a half-open socket
    /// would park the worker indefinitely while it holds its advisory lock,
    /// preventing any standby from taking over.
    pub fn build(cfg: &RpcConfig) -> Result<Arc<Self>, IngesterError> {
        let url: Url = cfg
            .url
            .parse()
            .map_err(|e: url::ParseError| IngesterError::Config(format!("rpc_url: {}", e)))?;
        let http_client = Client::builder()
            .timeout(cfg.request_timeout)
            .connect_timeout(cfg.connect_timeout)
            .build()
            .map_err(|e| IngesterError::Config(format!("http client: {}", e)))?;
        let is_local = matches!(url.host_str(), Some("localhost") | Some("127.0.0.1"));
        let transport = Http::with_client(http_client, url);
        let inner = ProviderBuilder::new().on_client(RpcClient::new(transport, is_local));
        Ok(Arc::new(Self {
            inner,
            meta_permits: Arc::new(Semaphore::new(cfg.meta_concurrency.max(1))),
            chain_id: cfg.chain_id,
        }))
    }
}

/// How many block-metadata futures are polled at once.
///
/// Not the request cap — that is [`HttpRpc::meta_permits`], which is shared
/// across concurrent chunks. This only bounds how many futures are parked on the
/// semaphore, so it is generous.
const META_POLL_WIDTH: usize = 256;

/// Wire method names, used for both the request and its counter label.
const M_BLOCK_NUMBER: &str = "eth_blockNumber";
const M_GET_LOGS: &str = "eth_getLogs";
const M_GET_BLOCK: &str = "eth_getBlockByNumber";

/// Count one call and map its failure onto the taxonomy the callers act on.
///
/// A free function rather than a method: `fetch_block_meta` fans out over clones
/// of the inner provider and has no `&self` to reach for inside the stream.
fn observe<T, E: std::fmt::Display>(
    method: &'static str,
    chain_id: i64,
    r: Result<T, E>,
) -> Result<T, IngesterError> {
    record_rpc_call(method, chain_id);
    r.map_err(|e| {
        let class = classify(e);
        record_rpc_error(class.label(), chain_id);
        IngesterError::from(class)
    })
}

#[async_trait]
impl ChainRpc for HttpRpc {
    async fn tip(&self) -> Result<u64, IngesterError> {
        observe(
            M_BLOCK_NUMBER,
            self.chain_id,
            self.inner.get_block_number().await,
        )
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
        observe(
            M_GET_LOGS,
            self.chain_id,
            self.inner.get_logs(&filter).await,
        )
    }

    async fn fetch_block_meta(
        &self,
        block_numbers: &[u64],
    ) -> Result<HashMap<u64, BlockMeta>, IngesterError> {
        use futures::stream::{self, StreamExt, TryStreamExt};

        // Bounded fan-out. A large backfill chunk can touch thousands of distinct
        // blocks, and issuing one request per block at once invites rate limiting
        // and socket exhaustion. The bound is the provider's semaphore, so
        // concurrent chunks share one budget instead of each getting their own.
        let chain_id = self.chain_id;
        let permits = &self.meta_permits;
        stream::iter(block_numbers.iter().copied().map(|n| {
            let p = self.inner.clone();
            async move {
                let _permit = permits
                    .acquire()
                    .await
                    .map_err(|e| IngesterError::Rpc(RpcError::Other(e.to_string())))?;
                let blk = raw_block(&p, chain_id, n).await?;
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
        .buffer_unordered(META_POLL_WIDTH)
        .try_collect()
        .await
    }

    async fn block_hash_at(&self, n: u64) -> Result<Option<B256>, IngesterError> {
        let Some(blk) = raw_block(&self.inner, self.chain_id, n).await? else {
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
    chain_id: i64,
    n: u64,
) -> Result<Option<serde_json::Value>, IngesterError> {
    observe(
        M_GET_BLOCK,
        chain_id,
        provider
            .raw_request(M_GET_BLOCK.into(), (format!("0x{n:x}"), false))
            .await,
    )
}

/// Read a `0x`-prefixed quantity from a JSON block object.
fn hex_u64(v: &serde_json::Value, key: &str) -> Option<u64> {
    let s = v.get(key)?.as_str()?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}
