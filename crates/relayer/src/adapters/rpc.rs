//! One chain's HTTP JSON-RPC endpoint.
//!
//! Alloy's filler stack produces deeply-generic provider types that resist
//! `dyn Provider` boxing, so callers still build a provider per request. This
//! holds everything underneath it that is worth keeping: the parsed URL and a
//! `reqwest::Client` whose clones share a connection pool. Without the shared
//! client every request pays a fresh TCP + TLS handshake.

use crate::domain::error::{AppError, AppResult};
use alloy::rpc::client::RpcClient;
use alloy::transports::http::Http;
use alloy::transports::http::reqwest::Url;

pub type HttpTransport = Http<reqwest::Client>;

#[derive(Debug, Clone)]
pub struct RpcEndpoint {
    url: Url,
    http: reqwest::Client,
}

impl RpcEndpoint {
    pub fn new(rpc_url: &str) -> AppResult<Self> {
        let url: Url = rpc_url
            .parse()
            .map_err(|e: url::ParseError| AppError::Internal(format!("rpc url: {e}")))?;
        Ok(Self {
            url,
            http: reqwest::Client::new(),
        })
    }

    /// A fresh RPC client over the shared connection pool. Feed it to
    /// `ProviderBuilder::on_client`.
    pub fn client(&self) -> RpcClient<HttpTransport> {
        RpcClient::new(
            Http::with_client(self.http.clone(), self.url.clone()),
            false,
        )
    }
}
