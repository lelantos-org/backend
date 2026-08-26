//! One chain's HTTP JSON-RPC endpoint.
//!
//! Holds what sits underneath a provider: the parsed URL and a `reqwest::Client`
//! whose clones share a connection pool. Without the shared client every request
//! pays a fresh TCP and TLS handshake.

use crate::domain::error::{AppError, AppResult};
use alloy::rpc::client::{ClientBuilder, RpcClient};
use alloy::transports::http::Http;
use alloy::transports::http::reqwest::Url;
use alloy::transports::layers::{RetryBackoffLayer, RetryBackoffService};
use std::time::Duration;

/// The transport every provider in this crate is built on. Named because the
/// retry layer wraps it and `Submitter` must spell its provider type out to hold
/// it in a field.
pub type HttpTransport = RetryBackoffService<Http<reqwest::Client>>;

/// Deadline for a single JSON-RPC call.
///
/// Every call on the submission path is one fast round trip, so this sits well
/// above a slow `eth_estimateGas` and well below the OS-level TCP timeout that
/// would otherwise govern. It is required because the per-chain tree-mirror mutex
/// is held across submission: an untimed call against a hung node would hold that
/// chain's mutex, and every spend, swap and flush queued behind it, indefinitely.
///
/// Unrelated to `receipt_timeout_s`, which bounds a loop of short polls rather
/// than any single call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Deadline for reaching the node at all, as opposed to hearing back from it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Retry budget for a call the node failed to answer.
///
/// Without retries a single transient 5xx fails the whole submission and rolls
/// the tree mirror back. `RateLimitRetryPolicy` is broader than its name: it
/// retries any transport-level error alloy marks retryable, not only 429s.
///
/// Retrying is safe on this path. Reads are idempotent, and the one write,
/// `eth_sendRawTransaction`, carries an already-signed transaction, so a resend
/// has the same hash and a node that already holds it answers "already known"
/// rather than broadcasting twice.
///
/// Worst case for one logical call is `REQUEST_TIMEOUT * (1 + RETRIES)` plus
/// backoff, which stays inside the router's own request deadline.
const RETRIES: u32 = 3;
const RETRY_BACKOFF_MS: u64 = 200;
/// Paces retries after a rate-limit response, set high enough not to throttle the
/// relayer's own call volume.
const COMPUTE_UNITS_PER_SECOND: u64 = 500;

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
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| AppError::Internal(format!("rpc http client: {e}")))?;
        Ok(Self { url, http })
    }

    /// A fresh RPC client over the shared connection pool. Pass it to
    /// `ProviderBuilder::on_client` once, at construction: the fillers built on
    /// top of it cache per-provider state.
    pub fn client(&self) -> RpcClient<HttpTransport> {
        /// Alloy's `is_local` flag, which only selects a default poll interval:
        /// 250 ms local, 7 s remote. Submitters override it, so leaving it remote
        /// keeps the conservative default for those that do not.
        const IS_LOCAL: bool = false;

        ClientBuilder::default()
            .layer(RetryBackoffLayer::new(
                RETRIES,
                RETRY_BACKOFF_MS,
                COMPUTE_UNITS_PER_SECOND,
            ))
            .transport(
                Http::with_client(self.http.clone(), self.url.clone()),
                IS_LOCAL,
            )
    }
}
